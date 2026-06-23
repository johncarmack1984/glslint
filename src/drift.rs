//! Cross-check a module's GLSL UBO block against the `uniformTypes` declared for
//! it in JS/TS. luma keeps these two in sync by hand (the `.glsl` block packs the
//! std140 layout; `uniformTypes` tells luma how to fill it), and nothing else can
//! see both sides at once. This warns when they drift: a member on one side but
//! not the other, or a type mismatch.
//!
//! It is deliberately conservative — a heuristic scan of the JS, not a parser. If
//! it can't confidently read the `uniformTypes` object, it stays silent rather
//! than risk a false positive.

use crate::diagnostics::{Diag, Severity};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Drift-check `glsl_path` (a module's UBO block) against its `types` JS file.
pub fn check(glsl_path: &Path, glsl_source: &str, module_name: &str, types_path: &Path) -> Vec<Diag> {
    let Ok(js) = std::fs::read_to_string(types_path) else {
        return Vec::new();
    };
    let js_label = types_path.file_name().and_then(|n| n.to_str()).unwrap_or("uniformTypes");
    check_against(glsl_path, glsl_source, module_name, &js, js_label)
}

/// The file-free core, for testing.
fn check_against(glsl_path: &Path, glsl_source: &str, module_name: &str, js: &str, js_label: &str) -> Vec<Diag> {
    let Some(js_types) = extract_uniform_types(js, module_name) else {
        return Vec::new(); // couldn't read the JS confidently — say nothing
    };
    let glsl = glsl_members(glsl_source);
    if glsl.is_empty() || js_types.is_empty() {
        return Vec::new();
    }

    let js_map: HashMap<&str, &str> = js_types.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let glsl_names: HashSet<&str> = glsl.iter().map(|(n, ..)| n.as_str()).collect();
    let mut out = Vec::new();

    for (name, gtype, line, col) in &glsl {
        match js_map.get(name.as_str()) {
            None => out.push(warn(
                glsl_path,
                *line,
                *col,
                name.chars().count() as u32,
                format!("`{name}` is in the GLSL block but not in `{js_label}` uniformTypes"),
            )),
            Some(luma) => {
                if let Some(expected) = luma_to_glsl(luma) {
                    if expected != gtype {
                        out.push(warn(
                            glsl_path,
                            *line,
                            *col,
                            name.chars().count() as u32,
                            format!("type drift: GLSL `{gtype} {name}` vs uniformTypes `{name}: '{luma}'` (expected `{expected}`)"),
                        ));
                    }
                }
            }
        }
    }
    for (name, _) in &js_types {
        if !glsl_names.contains(name.as_str()) {
            out.push(warn(
                glsl_path,
                1,
                1,
                1,
                format!("`{js_label}` declares `{name}`, but the GLSL block has no such member"),
            ));
        }
    }
    out
}

fn warn(path: &Path, line: u32, col: u32, len: u32, message: String) -> Diag {
    Diag { path: path.to_path_buf(), line, col, len, severity: Severity::Warning, message, source: "drift" }
}

/// Members of the first `uniform … { … }` block: `(name, glsl_type, line, col)`.
fn glsl_members(source: &str) -> Vec<(String, String, u32, u32)> {
    let mut out = Vec::new();
    let mut inside = false;
    for (i, line) in source.lines().enumerate() {
        let t = line.trim();
        if !inside {
            if t.contains("uniform") && t.ends_with('{') {
                inside = true;
            }
            continue;
        }
        if t.starts_with('}') {
            break;
        }
        if let Some(body) = t.strip_suffix(';') {
            let mut it = body.split_whitespace();
            if let (Some(ty), Some(name_raw), None) = (it.next(), it.next(), it.next()) {
                let name = name_raw.split('[').next().unwrap_or(name_raw);
                let col = line.find(name).map(|b| b as u32 + 1).unwrap_or(1);
                out.push((name.to_string(), ty.to_string(), i as u32 + 1, col));
            }
        }
    }
    out
}

/// Heuristically pull `{ key: 'type', … }` from the `uniformTypes` of the JS
/// declaration named `module_name`. `None` if it can't be located confidently.
fn extract_uniform_types(js: &str, module_name: &str) -> Option<Vec<(String, String)>> {
    let decl = decl_offset(js, module_name)?;
    let kw = js[decl..].find("uniformTypes").map(|o| o + decl)?;
    let open = js[kw..].find('{').map(|o| o + kw)?;
    let inner = brace_block(&js[open..])?;
    let pairs: Vec<(String, String)> = inner
        .lines()
        .filter_map(parse_pair)
        .collect();
    (!pairs.is_empty()).then_some(pairs)
}

/// Byte offset of a `const|let|var <name>` (optionally `export`) declaration.
fn decl_offset(js: &str, name: &str) -> Option<usize> {
    for kw in ["const ", "let ", "var "] {
        let needle = format!("{kw}{name}");
        let mut from = 0;
        while let Some(rel) = js[from..].find(&needle) {
            let start = from + rel;
            let after = start + needle.len();
            // require a word boundary after the name (so `windUniforms` != `windUniformsX`)
            let ok = js[after..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if ok {
                return Some(after);
            }
            from = start + 1;
        }
    }
    None
}

/// The content between a leading `{` and its matching `}`.
fn brace_block(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `  uMin: 'f32',` -> `("uMin", "f32")`.
fn parse_pair(line: &str) -> Option<(String, String)> {
    let (key, rest) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let q1 = rest.find('\'')?;
    let q2 = rest[q1 + 1..].find('\'')? + q1 + 1;
    Some((key.to_string(), rest[q1 + 1..q2].to_string()))
}

/// luma's WGSL-ish `uniformTypes` strings -> the GLSL type. `None` for shapes we
/// don't model (we then skip the type comparison rather than guess).
fn luma_to_glsl(t: &str) -> Option<&'static str> {
    Some(match t {
        "f32" => "float",
        "i32" => "int",
        "u32" => "uint",
        "bool" => "bool",
        "vec2<f32>" => "vec2",
        "vec3<f32>" => "vec3",
        "vec4<f32>" => "vec4",
        "vec2<i32>" => "ivec2",
        "vec3<i32>" => "ivec3",
        "vec4<i32>" => "ivec4",
        "vec2<u32>" => "uvec2",
        "vec3<u32>" => "uvec3",
        "vec4<u32>" => "uvec4",
        "mat2x2<f32>" => "mat2",
        "mat3x3<f32>" => "mat3",
        "mat4x4<f32>" => "mat4",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLSL: &str = "layout(std140) uniform windUniforms {\n  float uMin;\n  float uMax;\n  vec2 viewOffset;\n} wind;\n";
    const JS: &str = "export const windUniforms = {\n  name: 'wind',\n  uniformTypes: {\n    uMin: 'f32',\n    uMax: 'f32',\n    viewOffset: 'vec2<f32>',\n  },\n};\n";

    fn run(glsl: &str, js: &str) -> Vec<String> {
        check_against(Path::new("/p/windUniforms.glsl"), glsl, "windUniforms", js, "modules.ts")
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn in_sync_is_silent() {
        assert!(run(GLSL, JS).is_empty());
    }

    #[test]
    fn flags_member_only_in_glsl() {
        let glsl = GLSL.replace("} wind;", "  float maxSpeed;\n} wind;");
        let msgs = run(&glsl, JS);
        assert!(msgs.iter().any(|m| m.contains("maxSpeed") && m.contains("not in")));
    }

    #[test]
    fn flags_member_only_in_js() {
        let js = JS.replace("uMax: 'f32',", "uMax: 'f32',\n    randSeed: 'f32',");
        let msgs = run(GLSL, &js);
        assert!(msgs.iter().any(|m| m.contains("randSeed") && m.contains("no such member")));
    }

    #[test]
    fn flags_type_mismatch() {
        // GLSL says `vec2 viewOffset`, JS says it's an f32.
        let js = JS.replace("viewOffset: 'vec2<f32>'", "viewOffset: 'f32'");
        let msgs = run(GLSL, &js);
        assert!(msgs.iter().any(|m| m.contains("type drift") && m.contains("viewOffset") && m.contains("expected `float`")));
    }

    #[test]
    fn unreadable_js_is_silent() {
        // No `uniformTypes` object the scan can find -> no diagnostics.
        assert!(run(GLSL, "export const windUniforms = { name: 'wind' };").is_empty());
        // Different module name -> not found -> silent.
        assert!(check_against(Path::new("/p/x.glsl"), GLSL, "other", JS, "modules.ts").is_empty());
    }

    #[test]
    fn maps_luma_types() {
        assert_eq!(luma_to_glsl("f32"), Some("float"));
        assert_eq!(luma_to_glsl("vec3<f32>"), Some("vec3"));
        assert_eq!(luma_to_glsl("mat4x4<f32>"), Some("mat4"));
        assert_eq!(luma_to_glsl("weird"), None);
    }
}
