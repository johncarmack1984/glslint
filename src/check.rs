//! Drive glslangValidator — the Khronos GLSL reference compiler — over an
//! assembled unit and translate its `0:LINE` diagnostics back to the original
//! files via the line map.
//!
//! glslangValidator validates GLSL ES natively: `#version 300 es`, the combined
//! `sampler2D` type, and combined-sampler *function parameters* like
//! `windAt(sampler2D, vec2)` — none of which naga's Vulkan-GLSL frontend accepts.
//! So the assembler ships ES source through it verbatim, with no source rewrites.

use crate::assemble::{self, Assembled};
use crate::config::Config;
use crate::diagnostics::{Diag, Severity};
use crate::lints;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn check_file(path: &Path) -> anyhow::Result<Vec<Diag>> {
    let source = std::fs::read_to_string(path)?;
    Ok(check_source(path, &source))
}

pub fn check_source(path: &Path, source: &str) -> Vec<Diag> {
    let config = Config::resolve_for(path);
    let assembled = assemble::assemble(path, source, &config);

    let mut diags = check_assembled(&assembled);
    diags.extend(lints::run_lints(path, source));
    diags.sort_by(|a, b| {
        (a.path.as_path(), a.line, a.col).cmp(&(b.path.as_path(), b.line, b.col))
    });
    diags
}

fn check_assembled(a: &Assembled) -> Vec<Diag> {
    let run = match run_glslang(a) {
        Ok(run) => run,
        Err(RunError::NotFound) => {
            return vec![tool_error(
                a,
                "glslangValidator not found on PATH — install with `brew install glslang`, \
                 or set GLSLINT_GLSLANG to its path"
                    .to_string(),
            )];
        }
        Err(RunError::Io(e)) => {
            return vec![tool_error(a, format!("failed to run glslangValidator: {e}"))];
        }
    };

    let diags = parse_output(a, &run.output);
    // A non-zero exit with nothing parseable must never pass silently (a span we
    // can't map, an unexpected message format, etc.).
    if diags.is_empty() && !run.success {
        let detail = run
            .output
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("ERROR:") || l.starts_with("WARNING:"))
            .find(|l| !l.contains("compilation error") && !l.contains("compilation warning"))
            .unwrap_or("unknown error");
        return vec![tool_error(a, format!("glslangValidator failed: {detail}"))];
    }
    diags
}

enum RunError {
    NotFound,
    Io(std::io::Error),
}

struct GlslangRun {
    output: String,
    success: bool,
}

/// Run glslangValidator on the assembled source via stdin. Tries `glslangValidator`
/// then `glslang` (the renamed binary), or `$GLSLINT_GLSLANG` when set.
fn run_glslang(a: &Assembled) -> Result<GlslangRun, RunError> {
    for bin in glslang_candidates() {
        let mut child = match Command::new(&bin)
            .arg("--stdin")
            .arg("-S")
            .arg(a.stage.glslang_stage())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            // Binary absent: fall through to the next candidate.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(RunError::Io(e)),
        };

        // Write the whole (tiny) unit and close stdin before reading stdout —
        // glslang consumes all input before emitting, so this can't deadlock.
        {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            if let Err(e) = stdin.write_all(a.source.as_bytes()) {
                return Err(RunError::Io(e));
            }
        }
        let out = child.wait_with_output().map_err(RunError::Io)?;
        // Diagnostics land on stdout; fold in stderr defensively.
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&out.stderr));
        return Ok(GlslangRun { output, success: out.status.success() });
    }
    Err(RunError::NotFound)
}

fn glslang_candidates() -> Vec<String> {
    if let Some(bin) = std::env::var_os("GLSLINT_GLSLANG") {
        return vec![bin.to_string_lossy().into_owned()];
    }
    vec!["glslangValidator".to_string(), "glslang".to_string()]
}

/// Parse glslangValidator's diagnostics and map each home.
///
/// Format: `ERROR: <str>:<line>: 'token' : message`, where `<str>` is always 0
/// for our single stdin unit. Lines with no `<str>:<line>` prefix are file-level
/// (e.g. a bad `#version`) and surface as a fallback when nothing else maps.
fn parse_output(a: &Assembled, output: &str) -> Vec<Diag> {
    let mut mapped = Vec::new();
    let mut fileless = Vec::new();

    for line in output.lines() {
        let (severity, rest) = if let Some(r) = line.strip_prefix("ERROR: ") {
            (Severity::Error, r)
        } else if let Some(r) = line.strip_prefix("WARNING: ") {
            (Severity::Warning, r)
        } else {
            continue;
        };

        match parse_located(rest) {
            Some((lineno, token, msg)) => {
                // Parse-phase cascade terminator — drop it; the real error precedes.
                // (Semantic failures cascade differently — onto the same source
                // line — and are collapsed by `collapse_per_line` below.)
                if msg.contains("compilation terminated") {
                    continue;
                }
                if let Some(d) = map_located(a, lineno, token.as_deref(), severity, msg) {
                    mapped.push(d);
                }
            }
            None => {
                let msg = rest.trim();
                // Drop the per-run summary ("2 compilation errors. ...").
                if msg.contains("compilation error") || msg.contains("compilation warning") {
                    continue;
                }
                fileless.push((severity, msg.to_string()));
            }
        }
    }

    if !mapped.is_empty() {
        return collapse_per_line(mapped);
    }
    // Nothing mapped: surface any file-level messages, pinned to line 1.
    fileless
        .into_iter()
        .map(|(severity, message)| Diag {
            path: a.target.clone(),
            line: 1,
            col: 1,
            len: 1,
            severity,
            message,
            source: "glslang",
        })
        .collect()
}

/// Parse the `<str>:<line>: 'token' : message` body of a glslang diagnostic into
/// `(line, token, message)`. The message is kept verbatim (glslang's exact
/// wording, leading `'token'` and all); the token is split out only to refine the
/// column. Returns `None` when there's no `<str>:<line>` prefix (a file-level
/// message like "version not supported").
fn parse_located(rest: &str) -> Option<(u32, Option<String>, String)> {
    let mut parts = rest.splitn(3, ':');
    // Parsing the leading `<str>` index as a number is what distinguishes a
    // located diagnostic from a file-level one.
    let _str_no: u32 = parts.next()?.trim().parse().ok()?;
    let lineno: u32 = parts.next()?.trim().parse().ok()?;
    let message = parts.next()?.trim().to_string();

    // The offending token, when glslang quoted a non-empty one.
    let token = message
        .strip_prefix('\'')
        .and_then(|after| after.find('\'').map(|end| after[..end].to_string()))
        .filter(|t| !t.is_empty());

    Some((lineno, token, message))
}

/// Map a glslang `0:LINE` location to the original file, refining the column from
/// the offending token when it appears verbatim on the line. An error on a line
/// glslint itself injected is surfaced at the target's line 1 rather than dropped,
/// so a regression in our own prelude/setup can't pass silently.
fn map_located(
    a: &Assembled,
    asm_line: u32,
    token: Option<&str>,
    severity: Severity,
    message: String,
) -> Option<Diag> {
    let idx = asm_line.checked_sub(1)? as usize;
    match a.map.get(idx)? {
        // A line from a real source file: point the diagnostic there. The assembled
        // line is a verbatim copy of the original (no per-line rewrites), so a token
        // column found here is valid against the original too.
        Some(loc) => {
            let (col, len) = a
                .source
                .lines()
                .nth(idx)
                .and_then(|text| locate_token(text, token))
                .unwrap_or((1, 1));
            Some(Diag {
                path: loc.path.clone(),
                line: loc.line,
                col,
                len,
                severity,
                message: truncate_message(message),
                source: "glslang",
            })
        }
        // A line glslint injected (version / precision / prelude / wrapper main).
        // Clean in normal operation; if it errors, that's a regression in our own
        // code — surface it at line 1 instead of silently dropping it.
        None => Some(Diag {
            path: a.target.clone(),
            line: 1,
            col: 1,
            len: 1,
            severity,
            message: truncate_message(format!("(in glslint-injected code) {message}")),
            source: "glslang",
        }),
    }
}

/// Column (1-based char) and length of `token` within `line`, when it's an
/// identifier-like token present verbatim. `None` keeps the caller at column 1 —
/// we never point at a guessed location.
fn locate_token(line: &str, token: Option<&str>) -> Option<(u32, u32)> {
    let tok = token?;
    let first = tok.chars().next()?;
    if !(first.is_alphanumeric() || first == '_') {
        return None; // operators / punctuation — don't hunt for a stray match
    }
    // First *whole-word* occurrence. glslang doesn't say which occurrence it meant,
    // but matching on word boundaries at least avoids underlining `pos` inside
    // `position` or `speed` inside `speedFactor`.
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(tok) {
        let start = from + rel;
        let end = start + tok.len();
        let left_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let right_ok = end == line.len() || !is_word_byte(bytes[end]);
        if left_ok && right_ok {
            let col = line[..start].chars().count() as u32 + 1;
            return Some((col, tok.chars().count() as u32));
        }
        from = start + 1;
    }
    None
}

/// An ASCII identifier byte (`[A-Za-z0-9_]`). A multibyte UTF-8 byte is not one,
/// so a non-ASCII char correctly counts as a word boundary here.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Collapse glslang's per-line error cascade to its root cause. For a semantic
/// failure glslang does NOT stop at the first error: it emits the root cause and
/// then a string of *derived* errors (and sometimes exact duplicates) on the same
/// source line — e.g. a bad UBO-member access yields `'speed' : no such field`
/// followed by a giant `'=' : cannot convert from <whole block type>`. glslang
/// emits the root cause first, so keeping the first diagnostic per `(path, line)`
/// drops the noise the user can't act on. Lint diagnostics are added later and
/// are deliberately left untouched.
fn collapse_per_line(diags: Vec<Diag>) -> Vec<Diag> {
    let mut seen = std::collections::HashSet::new();
    diags
        .into_iter()
        .filter(|d| seen.insert((d.path.clone(), d.line)))
        .collect()
}

/// Cap message length. glslang inlines full type definitions into some messages
/// (an entire `uniform block{...}` for a bad interface-block access), which is
/// unreadable in a terminal or an editor hover. Truncate on a char boundary.
fn truncate_message(message: String) -> String {
    const MAX: usize = 200;
    if message.chars().count() > MAX {
        let mut t: String = message.chars().take(MAX).collect();
        t.push('…');
        t
    } else {
        message
    }
}

/// A diagnostic for a tooling failure (validator missing or crashed), pinned to
/// the target's first line so it's visible in both the CLI and the editor.
fn tool_error(a: &Assembled, message: String) -> Diag {
    Diag {
        path: a.target.clone(),
        line: 1,
        col: 1,
        len: 1,
        severity: Severity::Error,
        message,
        source: "glslint",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assemble::{Assembled, Loc, Stage};
    use std::path::PathBuf;

    fn assembled(map: Vec<Option<Loc>>, source: &str) -> Assembled {
        Assembled {
            source: source.to_string(),
            stage: Stage::Fragment,
            map,
            target: PathBuf::from("/proj/draw.frag.glsl"),
            note: None,
        }
    }

    // --- parse_located: the `<str>:<line>: 'token' : message` grammar ---

    #[test]
    fn parse_located_extracts_line_token_and_verbatim_message() {
        let (line, token, msg) =
            parse_located("0:5: 'undefined_a' : undeclared identifier ").unwrap();
        assert_eq!(line, 5);
        assert_eq!(token.as_deref(), Some("undefined_a"));
        // The message is kept verbatim, glslang's leading 'token' and all.
        assert_eq!(msg, "'undefined_a' : undeclared identifier");
    }

    #[test]
    fn parse_located_empty_token_becomes_none() {
        let (line, token, msg) = parse_located("0:5: '' : compilation terminated ").unwrap();
        assert_eq!(line, 5);
        assert_eq!(token, None);
        assert!(msg.contains("compilation terminated"));
    }

    #[test]
    fn parse_located_file_level_messages_have_no_prefix() {
        // A bad `#version` and its follow-on carry no `0:LINE:` prefix → None,
        // so they route to the line-1 fallback rather than a bogus location.
        assert!(parse_located(
            "#version: only version 300, 310, and 320 support the es profile"
        )
        .is_none());
        assert!(parse_located("version not supported").is_none());
    }

    // --- locate_token: column refinement from the offending token ---

    #[test]
    fn locate_token_returns_char_column_and_length() {
        let line = "  float alpha = nope;";
        let (col, len) = locate_token(line, Some("nope")).unwrap();
        assert_eq!(len, 4);
        assert_eq!(col, line.find("nope").unwrap() as u32 + 1);
    }

    #[test]
    fn locate_token_column_is_char_based_not_byte_based() {
        // A multibyte char before the token: the column must count chars, not bytes
        // (the LSP/CLI both treat `col` as a character offset).
        let line = "x = café + bad;"; // 'é' is 2 bytes, 1 char
        let (col, _) = locate_token(line, Some("bad")).unwrap();
        let char_col = line.chars().position(|c| c == 'b').unwrap() as u32 + 1;
        let byte_col = line.find("bad").unwrap() as u32 + 1;
        assert_eq!(col, char_col);
        assert_ne!(col, byte_col);
    }

    #[test]
    fn locate_token_skips_operators_and_absent_tokens() {
        assert_eq!(locate_token("a = b;", Some("=")), None); // punctuation: not hunted
        assert_eq!(locate_token("float x;", Some("zzz")), None); // not present
        assert_eq!(locate_token("float x;", None), None); // no token quoted
    }

    #[test]
    fn locate_token_matches_whole_words_only() {
        // Skips `speed` inside `speedFactor`, lands on the standalone occurrence.
        let line = "float speedFactor; x = speed;";
        let (col, len) = locate_token(line, Some("speed")).unwrap();
        assert_eq!(len, 5);
        assert_eq!(col, line.rfind("speed").unwrap() as u32 + 1);
        // `pos` must not match inside `position`.
        assert_eq!(locate_token("vec2 position;", Some("pos")), None);
    }

    // --- map_located: translate an assembled line back to the original file ---

    #[test]
    fn map_located_surfaces_injected_code_errors_at_line_one() {
        // map[0] = None → glslint owns that assembled line. A real error there is a
        // regression in our injected code; surface it (don't drop it) at line 1.
        let a = assembled(vec![None], "#version 300 es\n");
        let d = map_located(&a, 1, None, Severity::Error, "boom".into()).unwrap();
        assert_eq!(d.line, 1);
        assert_eq!(d.path, a.target);
        assert!(d.message.contains("glslint-injected"));
    }

    #[test]
    fn map_located_retargets_to_the_injected_module_file() {
        // An error on an assembled line that came from an injected module must
        // point at THAT module, not the file under check.
        let module = PathBuf::from("/proj/windUniforms.glsl");
        let a = assembled(
            vec![None, Some(Loc { path: module.clone(), line: 3 })],
            "#version 300 es\nfloat uMax;\n",
        );
        let d = map_located(&a, 2, Some("uMax"), Severity::Error, "boom".into()).unwrap();
        assert_eq!(d.path, module);
        assert_eq!(d.line, 3);
    }

    #[test]
    fn map_located_truncates_giant_messages() {
        let a = assembled(
            vec![Some(Loc { path: PathBuf::from("/proj/x.frag.glsl"), line: 1 })],
            "x\n",
        );
        let giant = format!("'=' : cannot convert from {}", "a".repeat(500));
        let d = map_located(&a, 1, None, Severity::Error, giant).unwrap();
        assert!(d.message.chars().count() <= 201);
        assert!(d.message.ends_with('…'));
    }

    // --- parse_output: cascade collapse + file-level fallback ---

    #[test]
    fn parse_output_collapses_cascade_to_first_per_line() {
        // Root cause + derived giant message + terminator + summary, all from one
        // bad line → exactly one diagnostic (the root cause) survives.
        let a = assembled(
            vec![Some(Loc { path: PathBuf::from("/proj/x.frag.glsl"), line: 7 })],
            "ignored\n",
        );
        let out = "x.frag.glsl\n\
                   ERROR: 0:1: 'speed' : no such field in structure 'wind'\n\
                   ERROR: 0:1: '=' :  cannot convert from a-giant-block-type\n\
                   ERROR: 0:1: '' : compilation terminated \n\
                   ERROR: 1 compilation errors.  No code generated.\n";
        let diags = parse_output(&a, out);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("no such field"));
        assert_eq!(diags[0].line, 7);
    }

    #[test]
    fn parse_output_routes_file_level_errors_to_line_one() {
        let a = assembled(vec![None], "#version 999 es\n");
        let out = "stdin\n\
                   ERROR: #version: only version 300, 310, and 320 support the es profile\n\
                   ERROR: version not supported\n\
                   ERROR: 1 compilation errors.  No code generated.\n";
        let diags = parse_output(&a, out);
        assert!(!diags.is_empty());
        assert!(diags.iter().all(|d| d.line == 1));
        assert!(diags.iter().any(|d| d.message.contains("version")));
    }
}
