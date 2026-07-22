//! Extract GLSL written inline in JS/TS tagged template literals — the
//! `glsl`…`` tag and the `/* glsl */ `…`` marker form — so the checker and LSP
//! can lint shaders that live in `.ts`/`.js` files, not only in standalone
//! `.glsl` files. This is deck.gl/luma.gl's other common shape: many projects
//! keep the shader source right next to the `new Model(...)` call as a tagged
//! template rather than a `?raw` import.
//!
//! Like `derive.rs`, this is a heuristic scan, not a JS parser. It walks the
//! source tracking strings and comments so it doesn't mistake a backtick inside a
//! `"…"` string — or a `glsl` substring inside a longer identifier — for a shader,
//! and it records, for every line of the reconstructed GLSL, where that line
//! begins in the host file. A diagnostic on the shader therefore lands on the
//! right `.ts` line and column (see [`Embedded::map`]).
//!
//! One known gap: regex literals aren't lexed (telling `/` division from a `/…/`
//! regex needs the preceding token, which a non-parser doesn't track). A regex
//! that contains a backtick could swallow a following template — but only into a
//! *missed* shader, never a wrongly-reported one, and regex-with-backtick before a
//! `glsl` template is vanishingly rare, so the trade favors staying a simple scan.
//!
//! Interpolations (`${…}`) make a template an incomplete translation unit: the
//! injected value can declare symbols the shader uses, or consume ones it
//! declares, and we can't see either. Rather than validate a shader we can't
//! faithfully reconstruct (and risk reporting errors that are really our
//! substitution's fault), a template with any interpolation is checked with the
//! source-level lints only — those match the author's own tokens and don't need a
//! complete unit — and gets one informational note. The `${…}` is blanked to
//! spaces (newlines preserved) purely so the lints' line/column math stays exact.

use crate::assemble::Stage;
use std::path::Path;

/// A 1-based (line, column) position in the host `.ts`/`.js` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPos {
    pub line: u32,
    pub col: u32,
}

/// A GLSL shader found inside a tagged template literal.
pub struct Embedded {
    /// The reconstructed GLSL. Line continuations (`\` + newline) are applied and
    /// interpolations are blanked to spaces, so source line `n` corresponds to
    /// `line_map[n - 1]` in the host file and columns line up (except across a
    /// line-continuation, which is rare inside a shader body).
    pub source: String,
    /// Inferred shader stage. Always concrete (defaulting to fragment); the
    /// default is only reached when the shader uses no stage-specific builtin, so
    /// validating under it can't misreport one as undeclared.
    pub stage: Stage,
    /// Whether the template has its own `main`. `false` => a chunk, wrapped in a
    /// synthetic `main` (under `stage`) for a syntax-only pass.
    pub has_entry: bool,
    /// The binding this template is assigned to (`const vs = glsl`…``, the `vs:`
    /// of an object), when discoverable. Used for stage inference and to recover
    /// the shader's luma modules from a `new Model({ modules })` call.
    pub name: Option<String>,
    /// True when the template contained a `${…}` interpolation.
    pub has_interp: bool,
    /// Where each 1-based source line begins in the host file (`line_map[i]` is
    /// source line `i + 1`). Always at least one entry.
    pub line_map: Vec<HostPos>,
}

impl Embedded {
    /// Map a 1-based (line, col) within [`Embedded::source`] to the host file's
    /// 1-based (line, col). Lines past the map clamp to the last entry.
    pub fn map(&self, line: u32, col: u32) -> HostPos {
        let idx = (line.saturating_sub(1) as usize).min(self.line_map.len().saturating_sub(1));
        let base = self
            .line_map
            .get(idx)
            .copied()
            .unwrap_or(HostPos { line: 1, col: 1 });
        HostPos {
            line: base.line,
            col: base.col.saturating_add(col.saturating_sub(1)),
        }
    }
}

/// Whether `path` is a JS/TS file we should scan for embedded shaders (rather than
/// treat wholesale as GLSL).
pub fn is_js_ts(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

/// Find every `glsl`…`` / `/* glsl */ `…`` shader in a JS/TS source.
pub fn extract(source: &str) -> Vec<Embedded> {
    let cs: Vec<char> = source.chars().collect();
    let mut cur = Cursor::new(&cs);
    let mut out = Vec::new();

    while let Some(c) = cur.peek() {
        match c {
            '/' if cur.peek2() == Some('/') => skip_line_comment(&mut cur),
            '/' if cur.peek2() == Some('*') => {
                let anchor = cur.i;
                if skip_block_comment(&mut cur) {
                    // A `/* glsl */` marker: the next template (modulo whitespace)
                    // is a shader.
                    skip_ws(&mut cur);
                    if cur.peek() == Some('`') {
                        cur.bump();
                        push_embedded(&mut out, &mut cur, binding_before(&cs, anchor));
                    }
                }
            }
            '\'' | '"' => skip_string(&mut cur, c),
            '`' => skip_template(&mut cur), // an untagged template — skip it whole
            c if is_ident_start(c) => {
                let anchor = cur.i;
                let ident = read_ident(&mut cur);
                // `glsl`…`` — but not `x.glsl`…`` (a member access) or `glslify`.
                if ident == "glsl" && !preceded_by_dot(&cs, anchor) {
                    skip_ws(&mut cur);
                    if cur.peek() == Some('`') {
                        cur.bump();
                        push_embedded(&mut out, &mut cur, binding_before(&cs, anchor));
                    }
                }
            }
            _ => {
                cur.bump();
            }
        }
    }
    out
}

fn push_embedded(out: &mut Vec<Embedded>, cur: &mut Cursor, name: Option<String>) {
    let (source, line_map, has_interp) = read_template(cur);
    let (stage, has_entry) = infer_stage(name.as_deref(), &source);
    out.push(Embedded {
        source,
        stage,
        has_entry,
        name,
        has_interp,
        line_map,
    });
}

/// Read a template literal, cursor positioned just past the opening backtick,
/// building the reconstructed GLSL, its per-line host map, and whether it held an
/// interpolation. Leaves the cursor just past the closing backtick.
fn read_template(cur: &mut Cursor) -> (String, Vec<HostPos>, bool) {
    let mut out = String::new();
    let mut line_map = vec![cur.pos()]; // source line 1 begins here
    let mut has_interp = false;

    while let Some(c) = cur.peek() {
        match c {
            '`' => {
                cur.bump();
                break;
            }
            '\\' => {
                cur.bump();
                match cur.peek() {
                    // `\` + newline is a JS line continuation: the newline is
                    // removed, so keep building the *same* source line. If nothing
                    // has landed on this line yet (the common `glsl`\`<newline>…`
                    // idiom), the line really begins after the continuation, so
                    // move its host anchor there.
                    Some('\r') => {
                        let fresh = line_empty(&out);
                        cur.bump();
                        if cur.peek() == Some('\n') {
                            cur.bump();
                        }
                        if fresh {
                            reanchor(&mut line_map, cur.pos());
                        }
                    }
                    Some('\n') => {
                        let fresh = line_empty(&out);
                        cur.bump();
                        if fresh {
                            reanchor(&mut line_map, cur.pos());
                        }
                    }
                    Some('`') => {
                        out.push('`');
                        cur.bump();
                    }
                    Some('$') => {
                        out.push('$');
                        cur.bump();
                    }
                    Some('\\') => {
                        out.push('\\');
                        cur.bump();
                    }
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                        cur.bump();
                    }
                    None => out.push('\\'),
                }
            }
            '$' if cur.peek2() == Some('{') => {
                has_interp = true;
                blank_interpolation(cur, &mut out, &mut line_map);
            }
            '\n' => {
                out.push('\n');
                cur.bump();
                line_map.push(cur.pos());
            }
            '\r' => {
                // A bare CR is not a line break to `str::lines()`, which the
                // assembler uses, so recording a source line for it would desync
                // `line_map`. Emit it (a CRLF's `\r` is stripped downstream) but
                // let only the `\n` arm own the line entry.
                out.push('\r');
                cur.bump();
            }
            _ => {
                out.push(c);
                cur.bump();
            }
        }
    }
    (out, line_map, has_interp)
}

/// Whether nothing has been emitted yet on the current (last) source line.
fn line_empty(out: &str) -> bool {
    out.rsplit('\n').next().is_none_or(str::is_empty)
}

/// Move the current source line's host anchor to `pos` (used when a leading line
/// continuation means the line's content really starts past the physical newline).
fn reanchor(line_map: &mut [HostPos], pos: HostPos) {
    if let Some(last) = line_map.last_mut() {
        *last = pos;
    }
}

/// Consume a `${…}` interpolation, emitting a space for each non-newline char (so
/// columns stay put) and a real newline for each newline (so line numbers stay
/// put). Nested strings and templates are consumed so their braces/backticks
/// don't confuse the brace matching.
fn blank_interpolation(cur: &mut Cursor, out: &mut String, line_map: &mut Vec<HostPos>) {
    cur.bump(); // $
    out.push(' ');
    cur.bump(); // {
    out.push(' ');
    let mut depth = 1i32;
    while let Some(c) = cur.peek() {
        match c {
            '{' => {
                depth += 1;
                cur.bump();
                out.push(' ');
            }
            '}' => {
                depth -= 1;
                cur.bump();
                out.push(' ');
                if depth == 0 {
                    break;
                }
            }
            '\'' | '"' => blank_string(cur, out, line_map, c),
            '`' => blank_nested_template(cur, out, line_map),
            '\n' => {
                cur.bump();
                out.push('\n');
                line_map.push(cur.pos());
            }
            '\r' => {
                // Bare CR isn't a source-line break (see `read_template`).
                cur.bump();
                out.push('\r');
            }
            _ => {
                cur.bump();
                out.push(' ');
            }
        }
    }
}

fn blank_string(cur: &mut Cursor, out: &mut String, line_map: &mut Vec<HostPos>, quote: char) {
    cur.bump(); // opening quote
    out.push(' ');
    while let Some(c) = cur.peek() {
        match c {
            '\\' => {
                cur.bump();
                out.push(' ');
                if cur.peek().is_some() {
                    cur.bump();
                    out.push(' ');
                }
            }
            c if c == quote => {
                cur.bump();
                out.push(' ');
                break;
            }
            '\n' => {
                cur.bump();
                out.push('\n');
                line_map.push(cur.pos());
            }
            _ => {
                cur.bump();
                out.push(' ');
            }
        }
    }
}

/// Blank a template literal nested inside a `${…}` interpolation. Deliberately
/// simple: it stops at the first backtick and doesn't recurse into a *further*
/// `${…}` within it. That mis-pairs backticks in the (essentially never seen)
/// doubly-nested case, but it's harmless — the whole interpolation is blanked to
/// spaces either way, `has_interp` is already set (so glslang is skipped), and the
/// loop always makes progress, so there's no panic or wrong diagnostic.
fn blank_nested_template(cur: &mut Cursor, out: &mut String, line_map: &mut Vec<HostPos>) {
    cur.bump(); // opening backtick
    out.push(' ');
    while let Some(c) = cur.peek() {
        match c {
            '\\' => {
                cur.bump();
                out.push(' ');
                if let Some(n) = cur.peek() {
                    cur.bump();
                    if n == '\n' {
                        out.push('\n');
                        line_map.push(cur.pos());
                    } else {
                        out.push(' ');
                    }
                }
            }
            '`' => {
                cur.bump();
                out.push(' ');
                break;
            }
            '\n' => {
                cur.bump();
                out.push('\n');
                line_map.push(cur.pos());
            }
            '\r' => {
                // Bare CR isn't a source-line break (see `read_template`).
                cur.bump();
                out.push('\r');
            }
            _ => {
                cur.bump();
                out.push(' ');
            }
        }
    }
}

/// Builtins that only exist in one stage. Using one under the wrong stage is an
/// "undeclared identifier" error, so a shader that uses any of these tells us its
/// stage for certain — and every shader that uses *none* is safe to validate under
/// the fragment default, since there's no stage-specific builtin to misreport.
const VERTEX_BUILTINS: &[&str] = &[
    "gl_Position",
    "gl_PointSize",
    "gl_VertexID",
    "gl_InstanceID",
];
const FRAGMENT_BUILTINS: &[&str] = &[
    "gl_FragCoord",
    "gl_FragDepth",
    "gl_FrontFacing",
    "gl_PointCoord",
    "discard",
    "dFdx",
    "dFdy",
    "fwidth",
];
const COMPUTE_BUILTINS: &[&str] = &[
    "gl_GlobalInvocationID",
    "gl_LocalInvocationID",
    "gl_LocalInvocationIndex",
    "gl_WorkGroupID",
    "gl_NumWorkGroups",
];

/// Infer `(stage, has_entry)` from the shader's text and binding name. `has_entry`
/// is whether it has its own `main` (else it's a chunk, wrapped syntax-only). The
/// stage is decided by a stage-specific builtin first (authoritative), then the
/// binding name (a mere hint), then a fragment default — an order chosen so the
/// default and the name hint are only reached when no stage-specific builtin is
/// present, and therefore can't cause a wrong-stage false positive.
fn infer_stage(name: Option<&str>, src: &str) -> (Stage, bool) {
    // Scan the code, not the comments: a `gl_Position` or `main` in prose mustn't
    // sway the result.
    let code = strip_comments(src);
    let has_entry = has_entry_point(&code);
    let stage = builtin_stage(&code)
        .or_else(|| name_stage(name))
        .unwrap_or(Stage::Fragment);
    (stage, has_entry)
}

/// The stage a shader's builtins pin it to, if any use is stage-specific.
fn builtin_stage(code: &str) -> Option<Stage> {
    let uses = |set: &[&str]| set.iter().any(|b| contains_word(code, b));
    if code.contains("local_size_x") || uses(COMPUTE_BUILTINS) {
        Some(Stage::Compute)
    } else if uses(VERTEX_BUILTINS) {
        Some(Stage::Vertex)
    } else if uses(FRAGMENT_BUILTINS) {
        Some(Stage::Fragment)
    } else {
        None
    }
}

/// A stage hinted by the binding name (`draw.vert`/`vs`/`drawVs` → vertex, and so
/// on). Only a hint — reached only when no stage-specific builtin decided it, so a
/// misfire here can't cause a wrong-stage false positive.
fn name_stage(name: Option<&str>) -> Option<Stage> {
    let n = name?.to_ascii_lowercase();
    if n.contains("vert") || n == "vs" || n.ends_with("vs") || n.ends_with("_vs") {
        Some(Stage::Vertex)
    } else if n.contains("frag") || n == "fs" || n.ends_with("fs") || n.ends_with("_fs") {
        Some(Stage::Fragment)
    } else if n.contains("comp") || n.contains("compute") {
        Some(Stage::Compute)
    } else {
        None
    }
}

/// Whether the code defines an entry point — a `void main` function. Just the word
/// `main` isn't enough (it could be an identifier); it must follow `void`.
fn has_entry_point(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(rel) = code[from..].find("main") {
        let start = from + rel;
        let end = start + 4;
        let left = start == 0 || !is_word_byte(bytes[start - 1]);
        let right = end == bytes.len() || !is_word_byte(bytes[end]);
        if left && right {
            // The `main` must be a `void main` function, and that `void` a whole
            // word (not `xvoid`). `ends_with("void")` guarantees `len >= 4`.
            let before = code[..start].trim_end();
            if before.ends_with("void") {
                let vstart = before.len() - 4;
                if vstart == 0 || !is_word_byte(before.as_bytes()[vstart - 1]) {
                    return true;
                }
            }
        }
        from = start + 1;
    }
    false
}

/// The binding a template is assigned to: the identifier before `= ` or `: `
/// immediately preceding position `i` (the start of the `glsl` tag or `/* glsl */`
/// marker). `None` when there's no simple assignment target.
fn binding_before(cs: &[char], i: usize) -> Option<String> {
    let mut i = skip_ws_back(cs, i);
    if i == 0 {
        return None;
    }
    let op = cs[i - 1];
    if op != '=' && op != ':' {
        return None;
    }
    // A `=` that's really part of `==`, `!=`, `<=`, `>=` isn't an assignment.
    if op == '=' && i >= 2 && matches!(cs[i - 2], '=' | '!' | '<' | '>') {
        return None;
    }
    i -= 1;
    i = skip_ws_back(cs, i);
    let end = i;
    while i > 0 && is_ident_char(cs[i - 1]) {
        i -= 1;
    }
    if i == end {
        return None;
    }
    let name: String = cs[i..end].iter().collect();
    is_ident_start(name.chars().next()?).then_some(name)
}

/// Whether the token starting at `i` is preceded (modulo whitespace) by a `.` —
/// i.e. it's a member access like `shaders.glsl`, not a bare `glsl` tag.
fn preceded_by_dot(cs: &[char], i: usize) -> bool {
    let i = skip_ws_back(cs, i);
    i > 0 && cs[i - 1] == '.'
}

fn skip_ws_back(cs: &[char], mut i: usize) -> usize {
    while i > 0 && cs[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

// --- the cursor and low-level skippers -------------------------------------

/// A char-based cursor over the source, tracking 1-based line/column so every
/// position is a host location without a second pass.
struct Cursor<'a> {
    cs: &'a [char],
    i: usize,
    line: u32,
    col: u32,
}

impl<'a> Cursor<'a> {
    fn new(cs: &'a [char]) -> Self {
        Cursor {
            cs,
            i: 0,
            line: 1,
            col: 1,
        }
    }
    fn peek(&self) -> Option<char> {
        self.cs.get(self.i).copied()
    }
    fn peek2(&self) -> Option<char> {
        self.cs.get(self.i + 1).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.cs.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }
    fn pos(&self) -> HostPos {
        HostPos {
            line: self.line,
            col: self.col,
        }
    }
}

fn read_ident(cur: &mut Cursor) -> String {
    let mut s = String::new();
    while let Some(c) = cur.peek() {
        if is_ident_char(c) {
            s.push(c);
            cur.bump();
        } else {
            break;
        }
    }
    s
}

/// Skip whitespace (including newlines — `glsl` and its backtick may sit on
/// different lines). Comments are not skipped here.
fn skip_ws(cur: &mut Cursor) {
    while matches!(cur.peek(), Some(c) if c.is_whitespace()) {
        cur.bump();
    }
}

fn skip_line_comment(cur: &mut Cursor) {
    cur.bump(); // /
    cur.bump(); // /
    while let Some(c) = cur.peek() {
        if c == '\n' {
            break;
        }
        cur.bump();
    }
}

/// Skip a `/* … */` comment, returning whether its content is exactly `glsl`
/// (case-insensitive) — the `/* glsl */` shader marker.
fn skip_block_comment(cur: &mut Cursor) -> bool {
    cur.bump(); // /
    cur.bump(); // *
    let start = cur.i;
    while let Some(c) = cur.peek() {
        if c == '*' && cur.peek2() == Some('/') {
            let content: String = cur.cs[start..cur.i].iter().collect();
            cur.bump(); // *
            cur.bump(); // /
            return content.trim().eq_ignore_ascii_case("glsl");
        }
        cur.bump();
    }
    false // unterminated
}

fn skip_string(cur: &mut Cursor, quote: char) {
    cur.bump(); // opening quote
    while let Some(c) = cur.peek() {
        match c {
            '\\' => {
                cur.bump();
                cur.bump();
            }
            c if c == quote => {
                cur.bump();
                return;
            }
            '\n' => return, // unterminated single-line string
            _ => {
                cur.bump();
            }
        }
    }
}

/// Skip a whole template literal, including any `${…}` interpolations (which can
/// nest strings and further templates), so its contents can't be misread as code.
fn skip_template(cur: &mut Cursor) {
    cur.bump(); // opening backtick
    while let Some(c) = cur.peek() {
        match c {
            '\\' => {
                cur.bump();
                cur.bump();
            }
            '`' => {
                cur.bump();
                return;
            }
            '$' if cur.peek2() == Some('{') => skip_interpolation(cur),
            _ => {
                cur.bump();
            }
        }
    }
}

fn skip_interpolation(cur: &mut Cursor) {
    cur.bump(); // $
    cur.bump(); // {
    let mut depth = 1i32;
    while let Some(c) = cur.peek() {
        match c {
            '{' => {
                depth += 1;
                cur.bump();
            }
            '}' => {
                depth -= 1;
                cur.bump();
                if depth == 0 {
                    return;
                }
            }
            '\'' | '"' => skip_string(cur, c),
            '`' => skip_template(cur),
            '\\' => {
                cur.bump();
                cur.bump();
            }
            _ => {
                cur.bump();
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Whether `word` appears in `text` on identifier boundaries (so `main` doesn't
/// match inside `domain`).
fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(word) {
        let start = from + rel;
        let end = start + word.len();
        let left = start == 0 || !is_word_byte(bytes[start - 1]);
        let right = end == bytes.len() || !is_word_byte(bytes[end]);
        if left && right {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Blank GLSL `//` and `/* */` comments to whitespace, so a word in a comment
/// isn't mistaken for code during stage inference. Content is otherwise verbatim.
fn strip_comments(src: &str) -> String {
    let cs: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '/' && cs.get(i + 1) == Some(&'/') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
        } else if cs[i] == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(cs.len());
        } else {
            out.push(cs[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: unwrap IS the assertion
mod tests {
    use super::*;

    #[test]
    fn extracts_a_tagged_template_and_its_binding() {
        let src = "const fs = glsl`#version 300 es\nvoid main() {}\n`;\n";
        let e = extract(src);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name.as_deref(), Some("fs"));
        assert_eq!(e[0].stage, Stage::Fragment);
        assert!(e[0].has_entry);
        assert!(!e[0].has_interp);
        assert!(e[0].source.starts_with("#version 300 es"));
    }

    #[test]
    fn maps_first_line_column_past_the_backtick() {
        // Content starts on the same physical line as the tag, so line 1 carries a
        // column offset; later lines start at the host line's own column 1.
        let src = "const fs = glsl`#version 300 es\nout vec4 c;\nvoid main(){ c = nope; }`;\n";
        let e = &extract(src)[0];
        // `const fs = glsl` is 15 chars; the backtick is char 16; `#` is char 17.
        assert_eq!(e.map(1, 1), HostPos { line: 1, col: 17 });
        // Line 3 begins at host line 3, column 1 → columns pass through unchanged.
        assert_eq!(e.map(3, 14), HostPos { line: 3, col: 14 });
    }

    #[test]
    fn recognizes_the_block_comment_marker_form() {
        let src = "export const src =\n  /* glsl */ `#version 300 es\nvoid main() {}\n`;\n";
        let e = extract(src);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name.as_deref(), Some("src"));
        // The shader begins right after the backtick on host line 2.
        assert_eq!(e[0].line_map[0].line, 2);
    }

    #[test]
    fn infers_vertex_from_gl_position() {
        let src = "const s = glsl`#version 300 es\nvoid main() { gl_Position = vec4(0.0); }`;\n";
        let e = &extract(src)[0];
        assert_eq!(e.stage, Stage::Vertex);
        assert!(e.has_entry);
    }

    #[test]
    fn infers_vertex_from_a_vertex_only_builtin_without_gl_position() {
        // A transform-feedback vertex shader writes no `gl_Position`, but `gl_VertexID`
        // is vertex-only — so it must not default to fragment (which would report
        // `gl_VertexID` as undeclared on valid code).
        let src = "const update = glsl`#version 300 es\nin float a;\nout float b;\n\
                   void main() { b = a * float(gl_VertexID); }`;\n";
        let e = &extract(src)[0];
        assert_eq!(e.stage, Stage::Vertex);
        assert!(e.has_entry);
    }

    #[test]
    fn content_stage_beats_a_misleading_binding_name() {
        // Named `vs`, but it uses the fragment-only `gl_FragCoord`: the builtin wins,
        // so it validates as a fragment (where `gl_FragCoord` is declared).
        let src = "const vs = glsl`#version 300 es\nout vec4 c;\n\
                   void main() { c = gl_FragCoord; }`;\n";
        assert_eq!(extract(src)[0].stage, Stage::Fragment);
    }

    #[test]
    fn a_chunk_without_main_is_wrapped_under_its_builtin_stage() {
        // No `main` → a chunk (wrapped syntax-only). A vertex-only builtin still
        // pins the stage, so the wrap is a vertex shell, not a fragment one.
        let plain = "const chunk = glsl`float rand(vec2 p) { return 0.0; }`;\n";
        let e = &extract(plain)[0];
        assert!(!e.has_entry);
        assert_eq!(e.stage, Stage::Fragment); // no stage-specific builtin → default

        let vtx = "const vid = glsl`float vid() { return float(gl_VertexID); }`;\n";
        let e = &extract(vtx)[0];
        assert!(!e.has_entry);
        assert_eq!(e.stage, Stage::Vertex);
    }

    #[test]
    fn main_in_a_comment_is_not_an_entry_point() {
        // Only a `void main` in *code* counts; a comment mentioning it must not make
        // a chunk look like a stage shader (glslang would then want a real `main`).
        let src = "const chunk = glsl`// main helpers below\nfloat rand() { return 0.0; }`;\n";
        assert!(!extract(src)[0].has_entry);
    }

    #[test]
    fn interpolation_is_flagged_and_blanked_preserving_lines() {
        let src = "const fs = glsl`#version 300 es\n${chunk}\nvoid main() {}\n`;\n";
        let e = &extract(src)[0];
        assert!(e.has_interp);
        // The `${chunk}` line is blanked to spaces but the line count is preserved,
        // so `void main` is still source line 3 → host line 3.
        let lines: Vec<&str> = e.source.lines().collect();
        assert_eq!(lines[0], "#version 300 es");
        assert!(lines[1].trim().is_empty());
        assert_eq!(lines[2], "void main() {}");
        assert_eq!(e.map(3, 1), HostPos { line: 3, col: 1 });
    }

    #[test]
    fn leading_line_continuation_drops_the_first_newline() {
        // `glsl`\` + newline: JS removes the newline, so `#version` is source line 1.
        let src = "const fs = glsl`\\\n#version 300 es\nvoid main() {}`;\n";
        let e = &extract(src)[0];
        assert!(e.source.starts_with("#version 300 es"));
        // `#version` physically sits on host line 2.
        assert_eq!(e.map(1, 1).line, 2);
    }

    #[test]
    fn ignores_glsl_as_a_substring_or_member() {
        // `glslify(...)` is a call, `x.glsl` a member, `myglsl` a longer ident —
        // none is our tag.
        let src = "const a = glslify`x`;\nconst b = x.glsl`y`;\nconst myglsl = 1;\n";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn skips_backticks_inside_ordinary_strings() {
        let src = "const s = \"a `glsl` b\";\nconst t = 'more `glsl`';\n";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn finds_multiple_templates_in_one_file() {
        let src = "const vs = glsl`#version 300 es\nvoid main(){ gl_Position = vec4(0.0); }`;\n\
                   const fs = glsl`#version 300 es\nout vec4 c;\nvoid main(){ c = vec4(1.0); }`;\n";
        let e = extract(src);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].stage, Stage::Vertex);
        assert_eq!(e[1].stage, Stage::Fragment);
    }

    #[test]
    fn fixture_tagged_ts_extracts_two_shaders() {
        let e = extract(include_str!("../tests/fixtures/tagged.ts"));
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].name.as_deref(), Some("fs"));
        assert_eq!(e[0].stage, Stage::Fragment);
        assert_eq!(e[1].name.as_deref(), Some("vs"));
        assert_eq!(e[1].stage, Stage::Vertex);
        // The `fs` shader's first content line (`#version 300 es`) is host line 4.
        assert_eq!(e[0].line_map[0].line, 4);
    }
}
