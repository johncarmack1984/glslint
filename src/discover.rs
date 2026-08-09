//! Discover a project's shader-assembly *from the project itself*, so glslint
//! works on an unfamiliar repo without a per-ecosystem profile of hardcoded
//! filenames. Two universal signals, neither of which names maplibre (or any
//! ecosystem):
//!
//! - **Shared libraries.** A shader is a translation unit with a `main`; a sibling
//!   file with no `main` is a shared library the build concatenates in ahead of
//!   the shader (maplibre's `_prelude`/`_projection_*`, deck's UBO fragments,
//!   three's chunks). glslint splices those in, so `projectTile` &c. resolve
//!   without a single filename baked into the linter. Mutually-exclusive variants
//!   (mercator vs globe both define `projectTile`) are deduped by function name —
//!   the first wins.
//! - **Injected defines.** A build often injects `#define NAME value` from JS
//!   before compiling (maplibre's `NUM_ILLUMINATION_SOURCES`). glslint scans the
//!   project's own JS/TS for those `#define`s and supplies an `#ifndef`-guarded
//!   default, so the macro resolves. Because it defaults only names the project
//!   actually injects, a *typo* in a macro name is still flagged.
//!
//! This is the same instinct `derive.rs` already uses for luma `Model` calls and
//! `deck.rs` for node_modules: read the project's ground truth rather than encode
//! it.

use crate::assemble::{Stage, detect_stage};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Sibling `.glsl` shared libraries (no `main`) that apply to `stage`, in the
/// order they should be spliced, variant-deduped by function name. `target` (the
/// shader under check) is excluded.
pub fn shared_libraries(dir: &Path, stage: Stage, target: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // Deterministic order so variant-dedup (globe before mercator) is stable, and
    // a file may rely on symbols an earlier one declares.
    files.sort();

    let mut out = Vec::new();
    let mut seen_fns: HashSet<String> = HashSet::new();
    for p in files {
        if p.extension().and_then(|e| e.to_str()) != Some("glsl") {
            continue;
        }
        if same_path(&p, target) {
            continue;
        }
        // A library with a stage in its name must match; a stageless one is shared
        // across stages.
        if detect_stage(&p).is_some_and(|s| s != stage) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&p) else {
            continue;
        };
        if has_main(&content) {
            continue; // it's a shader, not a library
        }
        let fns = top_level_fn_names(&content);
        // A file that redefines a function an earlier library already provides is a
        // mutually-exclusive variant (globe vs mercator) — skip it.
        if fns.iter().any(|f| seen_fns.contains(f)) {
            continue;
        }
        seen_fns.extend(fns);
        out.push(p);
    }
    out
}

/// `#define NAME ${…}` macros the project's JS/TS injects into shaders at build
/// time. Walks up from `start` to the repo root and scans source files, returning
/// the concrete macro names (templated names like `HAS_UNIFORM_${name}` are skipped
/// — those are the data-driven flags glslint deliberately leaves undefined).
///
/// Memoized by repo root: the scan walks the whole tree, so a run that lints many
/// shaders in one project pays for it once.
pub fn injected_defines(start: &Path) -> Vec<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<String>>>> = OnceLock::new();

    let root = repo_root(start);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(hit) = guard.get(&root)
    {
        return hit.clone();
    }

    let mut names: HashSet<String> = HashSet::new();
    scan_defines(&root, &mut names, 0);
    let mut out: Vec<String> = names.into_iter().collect();
    out.sort();

    if let Ok(mut guard) = cache.lock() {
        guard.insert(root, out.clone());
    }
    out
}

fn scan_defines(dir: &Path, out: &mut HashSet<String>, depth: usize) {
    if depth > 12 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(name, "node_modules" | ".git" | "target" | "dist" | "build") {
                continue;
            }
            scan_defines(&p, out, depth + 1);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") // Skip generated shader-string files (`*.glsl.g.ts`): they embed the
                                                              // shaders' own `#define`s, which are not injected macros.
        ) && !name.ends_with(".glsl.g.ts")
            && !name.ends_with(".glsl.js")
            && let Ok(text) = std::fs::read_to_string(&p)
        {
            collect_define_names(&text, out);
        }
    }
}

/// Pull the names of *runtime-injected* macros out of `text`: `#define NAME ${…}`,
/// where the value is a template interpolation. That templated value is the actual
/// signature of a build-time-injected define (maplibre's
/// `#define NUM_ILLUMINATION_SOURCES ${…}`) — it's what distinguishes them from a
/// shader's own literal-valued `#define GAUSS_COEF 0.3989`, which must NOT be
/// defaulted (doing so redefines it and breaks the shader). Templated *names*
/// (`HAS_UNIFORM_${name}`) are skipped: the name can't be resolved to a concrete
/// macro, and those flags are meant to stay undefined.
fn collect_define_names(text: &str, out: &mut HashSet<String>) {
    for (i, _) in text.match_indices("#define ") {
        let rest = &text[i + "#define ".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let after = &rest[name.len()..];
        // Concrete uppercase name, followed by a whitespace-then-`${` value.
        if name.len() >= 2
            && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && after.trim_start().starts_with("${")
            && after.starts_with([' ', '\t'])
        {
            out.insert(name);
        }
    }
}

/// Walk up to the directory holding `.git` (the repo root), else the start dir.
fn repo_root(start: &Path) -> PathBuf {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

/// Whether `source` declares its own `main` — the universal shader-vs-library
/// signal.
fn has_main(source: &str) -> bool {
    source
        .lines()
        .any(|l| l.replace(char::is_whitespace, "").contains("voidmain("))
}

/// Names of functions defined at file scope: a line beginning in column 0 shaped
/// `<returnType> <name>(`. Deliberately conservative — indented lines (locals,
/// bodies) and control keywords are ignored.
fn top_level_fn_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        // File-scope only: no leading whitespace.
        if line.starts_with([' ', '\t']) || line.is_empty() {
            continue;
        }
        let Some(paren) = line.find('(') else {
            continue;
        };
        let head = &line[..paren];
        let mut toks = head.split_whitespace();
        let (Some(ret), Some(name)) = (toks.next(), toks.last()) else {
            continue;
        };
        // `<type> <name>(` — a plausible return type and a plausible identifier,
        // not a call or control-flow keyword.
        if is_ident(ret)
            && is_ident(name)
            && !matches!(name, "if" | "for" | "while" | "switch" | "return")
        {
            out.push(name.to_string());
        }
    }
    out
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn top_level_fn_names_finds_definitions_not_calls() {
        let src = "\
uniform mat4 u_m;
vec4 projectTile(vec2 p) {
    vec4 r = u_m * vec4(p, 0.0, 1.0);
    if (p.x > 0.0) { r.z = 1.0; }
    return r;
}
float projectLineThickness(float y) { return 1.0; }
";
        let fns = top_level_fn_names(src);
        assert!(fns.contains(&"projectTile".to_string()));
        assert!(fns.contains(&"projectLineThickness".to_string()));
        // Not the indented `if`, the call, the uniform, or the return.
        assert!(!fns.iter().any(|f| f == "if" || f == "u_m" || f == "vec4"));
    }

    #[test]
    fn collect_define_names_skips_templated_macros() {
        let mut out = HashSet::new();
        collect_define_names(
            "const d = [`#define NUM_ILLUMINATION_SOURCES ${x.length}`, `#define HAS_UNIFORM_${name}`];",
            &mut out,
        );
        assert!(out.contains("NUM_ILLUMINATION_SOURCES"));
        // `HAS_UNIFORM_` alone is short/partial and the `${` guard drops it.
        assert!(!out.iter().any(|n| n.starts_with("HAS_UNIFORM")));
    }
}
