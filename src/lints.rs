//! Lint passes that run on the original source text — checks beyond "does it
//! compile", in the spirit of clippy/biome. naga gives us the spec-conformance
//! pass; this is where opinionated rules live.
//!
//! Rules here must be zero-false-positive: they only fire on the author's own
//! lines (never injected code) and on patterns that are unambiguously wrong.

use crate::diagnostics::{Diag, Severity};
use std::path::Path;

pub fn run_lints(target: &Path, source: &str) -> Vec<Diag> {
    let mut out = Vec::new();
    es3_legacy(target, source, &mut out);
    out
}

/// `es3-legacy`: GLSL ES 1.00 builtins/qualifiers that are removed in
/// `#version 300 es`. These compile under ES2 but are errors under WebGL2 — the
/// classic migration footgun.
fn es3_legacy(target: &Path, source: &str, out: &mut Vec<Diag>) {
    let is_es3 = source.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("#version") && t.contains("300") && t.contains("es")
    });
    if !is_es3 {
        return;
    }

    const REMOVED: &[(&str, &str)] = &[
        ("gl_FragColor", "`gl_FragColor` was removed in GLSL ES 3.00 — declare `out vec4` and write to it"),
        ("gl_FragData", "`gl_FragData` was removed in GLSL ES 3.00 — use a user-declared `out` array"),
        ("texture2D(", "`texture2D()` was removed in GLSL ES 3.00 — use `texture()`"),
        ("texture2DLod(", "`texture2DLod()` was removed in GLSL ES 3.00 — use `textureLod()`"),
        ("textureCube(", "`textureCube()` was removed in GLSL ES 3.00 — use `texture()`"),
    ];
    const QUALIFIERS: &[(&str, &str)] = &[
        ("varying ", "`varying` is GLSL ES 1.00 — use `in`/`out` in ES 3.00"),
        ("attribute ", "`attribute` is GLSL ES 1.00 — use `in` in ES 3.00"),
    ];

    for (i, line) in source.lines().enumerate() {
        // Ignore anything after a line comment to avoid flagging prose.
        let code = line.split("//").next().unwrap_or(line);
        let lineno = i as u32 + 1;

        for (needle, msg) in REMOVED {
            if let Some(byte) = code.find(needle) {
                out.push(Diag {
                    path: target.to_path_buf(),
                    line: lineno,
                    col: char_col(code, byte),
                    len: needle.trim_end_matches('(').chars().count() as u32,
                    severity: Severity::Warning,
                    message: (*msg).to_string(),
                    source: "lint",
                });
            }
        }

        // Storage qualifiers only count as a declaration at line start.
        let trimmed = code.trim_start();
        for (needle, msg) in QUALIFIERS {
            if trimmed.starts_with(needle) {
                let indent = code.len() - trimmed.len();
                out.push(Diag {
                    path: target.to_path_buf(),
                    line: lineno,
                    col: char_col(code, indent),
                    len: needle.trim_end().chars().count() as u32,
                    severity: Severity::Warning,
                    message: (*msg).to_string(),
                    source: "lint",
                });
            }
        }
    }
}

/// Convert a byte offset within a line to a 1-based char column.
fn char_col(line: &str, byte: usize) -> u32 {
    line[..byte].chars().count() as u32 + 1
}
