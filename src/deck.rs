//! Resolve deck.gl's `project` builtins from node_modules instead of leaning on a
//! hand-written 4-function stub. deck ships the module GLSL as a JS template
//! string whose *bodies* interpolate JS constants and reference a `geometry`/
//! `project` UBO dependency — so the bodies can't be spliced as-is. But the
//! function *signatures* are clean GLSL, and the readable `src/*.glsl.ts` ships
//! in node_modules. So we extract the real signatures and:
//!   - generate type-correct, empty-body stubs for the validator (no dependency
//!     graph needed — an empty body references nothing), and
//!   - record each function's real declaration site, so hover shows the true
//!     signature and go-to-definition jumps into the deck source.

use crate::assemble::Loc;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectFn {
    pub name: String,
    pub signature: String,
    pub ret: String,
    pub loc: Loc,
}

const GLSL_TYPES: &[&str] = &[
    "void", "bool", "int", "uint", "float", "double", "vec2", "vec3", "vec4", "ivec2", "ivec3",
    "ivec4", "uvec2", "uvec3", "uvec4", "bvec2", "bvec3", "bvec4", "mat2x2", "mat3x3", "mat4x4",
    "mat2", "mat3", "mat4",
];

/// Extract deck's `project`/`project32` GLSL functions from node_modules, walking
/// up from `dir`. Empty when deck isn't installed (caller falls back to the stub).
pub fn project_fns(dir: &Path) -> Vec<ProjectFn> {
    let Some(core) = find_deck_core(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rel in [
        "src/shaderlib/project/project.glsl.ts",
        "src/shaderlib/project32/project32.ts",
    ] {
        let path = core.join(rel);
        if let Ok(text) = std::fs::read_to_string(&path) {
            extract_fns(&text, &path, &mut out);
        }
    }
    out
}

/// Type-correct, empty-body GLSL stubs for the assembled unit.
pub fn stubs(fns: &[ProjectFn]) -> String {
    let mut s = String::from("// deck.gl project builtins (signatures resolved from node_modules)\n");
    for f in fns {
        s.push_str(&f.signature);
        s.push_str(&stub_body(&f.ret));
        s.push('\n');
    }
    s
}

fn stub_body(ret: &str) -> String {
    let val = match ret {
        "void" => return " {}".to_string(),
        "bool" => "false".to_string(),
        "int" => "0".to_string(),
        "uint" => "0u".to_string(),
        "float" | "double" => "0.0".to_string(),
        t if t.starts_with("ivec") => format!("{t}(0)"),
        t if t.starts_with("uvec") => format!("{t}(0u)"),
        t if t.starts_with("bvec") => format!("{t}(false)"),
        t if t.starts_with("vec") || t.starts_with("mat") => format!("{t}(0.0)"),
        _ => "0.0".to_string(),
    };
    format!(" {{ return {val}; }}")
}

fn find_deck_core(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let core = d.join("node_modules/@deck.gl/core");
        if core.join("src/shaderlib/project/project.glsl.ts").is_file() {
            return Some(core);
        }
        dir = d.parent();
    }
    None
}

fn extract_fns(text: &str, path: &Path, out: &mut Vec<ProjectFn>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some((ret, rest)) = split_type(lines[i].trim_start()) {
            if let Some(open) = rest.find('(') {
                let name = &rest[..open];
                if rest.starts_with("project") && is_ident(name) {
                    if let Some((params, close_line)) = collect_paren(&lines, i, rest, open) {
                        out.push(ProjectFn {
                            name: name.to_string(),
                            signature: format!("{ret} {name}({params})"),
                            ret: ret.to_string(),
                            loc: Loc { path: path.to_path_buf(), line: i as u32 + 1 },
                        });
                        i = close_line + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

/// A leading known GLSL type, returning `(type, the rest after it)`.
fn split_type(s: &str) -> Option<(&'static str, &str)> {
    for t in GLSL_TYPES {
        if let Some(rest) = s.strip_prefix(t) {
            if rest.starts_with(char::is_whitespace) {
                return Some((t, rest.trim_start()));
            }
        }
    }
    None
}

/// Collect the parameter list from a `(` at `open` in `first` (the trimmed line
/// `start`), spanning lines until the parens balance. Returns the whitespace-
/// normalized params and the line index of the closing `)`.
fn collect_paren(lines: &[&str], start: usize, first: &str, open: usize) -> Option<(String, usize)> {
    let mut params = String::new();
    let mut depth = 0i32;
    let mut chunk = &first[open..];
    let mut li = start;
    loop {
        for c in chunk.chars() {
            match c {
                '(' => {
                    depth += 1;
                    if depth == 1 {
                        continue; // skip the outer '('
                    }
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((normalize_ws(&params), li));
                    }
                }
                _ => {}
            }
            params.push(c);
        }
        params.push(' '); // line break inside params
        li += 1;
        chunk = lines.get(li)?;
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_and_multiline_signatures() {
        let src = "export const x = `\n\
                   vec4 project_common_position_to_clipspace(vec4 position) {\n\
                     return vec4(0.0);\n\
                   }\n\
                   vec4 project_position_to_clipspace(\n\
                     vec3 position, vec3 position64Low, vec3 offset\n\
                   ) {\n\
                     return vec4(0.0);\n\
                   }\n\
                   fn project_wgsl_thing() -> f32 {}\n`;";
        let mut out = Vec::new();
        extract_fns(src, Path::new("/p/project.glsl.ts"), &mut out);
        let names: Vec<_> = out.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"project_common_position_to_clipspace"));
        let multi = out.iter().find(|f| f.name == "project_position_to_clipspace").unwrap();
        assert_eq!(multi.signature, "vec4 project_position_to_clipspace(vec3 position, vec3 position64Low, vec3 offset)");
        // The WGSL `fn ...` form is not a GLSL type -> ignored.
        assert!(!names.contains(&"project_wgsl_thing"));
    }

    #[test]
    fn generates_typed_stub_bodies() {
        let f = |ret: &str| ProjectFn { name: "f".into(), signature: format!("{ret} f()"), ret: ret.into(), loc: Loc { path: PathBuf::new(), line: 1 } };
        assert!(stubs(&[f("void")]).contains("void f() {}"));
        assert!(stubs(&[f("float")]).contains("float f() { return 0.0; }"));
        assert!(stubs(&[f("vec4")]).contains("vec4 f() { return vec4(0.0); }"));
        assert!(stubs(&[f("mat3")]).contains("mat3 f() { return mat3(0.0); }"));
        assert!(stubs(&[f("bool")]).contains("bool f() { return false; }"));
    }
}
