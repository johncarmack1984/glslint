//! `#include` resolution — the one cross-file mechanism GLSL actually standardizes.
//!
//! Desktop/Vulkan GLSL has `#include "file"` / `#include <file>` via the
//! `GL_GOOGLE_include_directive` (and `GL_ARB_shading_language_include`) extensions,
//! which glslang recognizes. But glslint feeds glslang over stdin, so glslang has no
//! base directory to resolve includes against ("could not process include
//! directive"). So glslint resolves them itself: it splices each included file in
//! place, recursively, mapping every spliced line back to its own file so a
//! diagnostic lands there — exactly as it does for luma modules and the discovered
//! shared library.
//!
//! Resolution is lenient and standard-agnostic: any `#include "path"` /
//! `#include <path>` whose target exists (relative to the including file) is
//! spliced, whether or not the `#extension` line is present. An include that can't
//! be resolved is left verbatim, so glslang reports it at the include site. A file
//! that (transitively) includes itself is spliced once; the cycle is broken.

use crate::assemble::{Loc, line_no};
use std::path::{Path, PathBuf};

/// The path inside a `#include "path"` or `#include <path>` line, or `None` if the
/// line isn't an include directive.
pub fn parse(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("#include")?;
    // Require a separator so `#includexyz` can't match.
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start();
    let close = match rest.as_bytes().first()? {
        b'"' => '"',
        b'<' => '>',
        _ => return None,
    };
    let inner = &rest[1..];
    let end = inner.find(close)?;
    Some(&inner[..end])
}

/// If `line` is an `#include` that resolves to a readable file (relative to `dir`),
/// return its lines — recursively expanded, each mapped to its own file. `None` for
/// a non-include line or an unresolvable include (left verbatim by the caller).
/// `seen` is the current include stack, canonicalized, for cycle-breaking.
pub fn expand(line: &str, dir: &Path, seen: &mut Vec<PathBuf>) -> Option<Vec<(String, Loc)>> {
    let rel = parse(line)?;
    let canon = dir.join(rel).canonicalize().ok()?;
    if seen.contains(&canon) {
        // Already on the include stack — break the cycle (or a diamond re-include).
        return Some(Vec::new());
    }
    let content = std::fs::read_to_string(&canon).ok()?;
    let inc_dir = canon.parent().unwrap_or(Path::new(".")).to_path_buf();

    seen.push(canon.clone());
    let mut out = Vec::new();
    for (i, l) in content.lines().enumerate() {
        match expand(l, &inc_dir, seen) {
            Some(nested) => out.extend(nested),
            None => out.push((
                l.to_string(),
                Loc {
                    path: canon.clone(),
                    line: line_no(i),
                },
            )),
        }
    }
    seen.pop();
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_quoted_and_angled_paths() {
        assert_eq!(parse("#include \"common.glsl\""), Some("common.glsl"));
        assert_eq!(parse("  #include <lib/util.glsl>"), Some("lib/util.glsl"));
        assert_eq!(parse("#include\t\"a.glsl\""), Some("a.glsl"));
        assert_eq!(parse("#version 300 es"), None);
        assert_eq!(parse("#includexyz \"a\""), None);
        assert_eq!(parse("// #include \"a.glsl\"".trim_start()), None);
    }

    #[test]
    fn expand_splices_recursively_and_breaks_cycles() {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "glslint-inc-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // a.glsl includes b.glsl; b.glsl includes a.glsl (cycle).
        std::fs::write(
            dir.join("a.glsl"),
            "float a() { return 1.0; }\n#include \"b.glsl\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.glsl"),
            "float b() { return 2.0; }\n#include \"a.glsl\"\n",
        )
        .unwrap();

        let mut seen = Vec::new();
        let out = expand("#include \"a.glsl\"", &dir, &mut seen).unwrap();
        let text: Vec<&str> = out.iter().map(|(l, _)| l.as_str()).collect();
        assert!(text.iter().any(|l| l.contains("float a()")));
        assert!(text.iter().any(|l| l.contains("float b()")));
        // The cycle back to a.glsl is broken — `a()` appears exactly once.
        assert_eq!(text.iter().filter(|l| l.contains("float a()")).count(), 1);
        // A spliced line maps back to the file it came from.
        let b_line = out.iter().find(|(l, _)| l.contains("float b()")).unwrap();
        assert!(b_line.1.path.ends_with("b.glsl"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unresolvable_include_is_left_for_the_caller() {
        let mut seen = Vec::new();
        assert!(
            expand(
                "#include \"nope-does-not-exist.glsl\"",
                Path::new("/tmp"),
                &mut seen
            )
            .is_none()
        );
    }
}
