//! A lightweight symbol scanner over the assembled unit, powering hover and
//! go-to-definition. Deliberately line-based (like the assembler and the lints),
//! not a full GLSL parser — it recognizes the shapes luma/deck shaders actually
//! use: UBO/interface blocks, top-level `uniform`/`in`/`out` declarations, and
//! function definitions. Every symbol carries its original `Loc` via the line
//! map, so a `wind.uMin` access in one file resolves to its declaration in the
//! injected `windUniforms.glsl` — the cross-module jump stock tools can't do.

use crate::assemble::{Assembled, Loc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Symbol {
    pub detail: String,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct Ubo {
    pub block_name: String,
    pub detail: String,
    pub loc: Loc,
    pub members: HashMap<String, Symbol>,
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    pub globals: HashMap<String, Symbol>,
    pub functions: HashMap<String, Symbol>,
    /// Keyed by the block *instance* name (e.g. `wind`), not the block type.
    pub ubos: HashMap<String, Ubo>,
}

/// A resolved hover / definition target.
#[derive(Debug, Clone)]
pub struct Hit {
    pub detail: String,
    pub note: Option<String>,
    pub loc: Loc,
}

const QUALIFIERS: &[&str] = &["uniform", "in", "out", "attribute", "varying", "const"];

/// Scan the assembled source into a symbol index. Synthetic lines (the injected
/// prelude/precision, which have no `Loc`) are skipped — we only index symbols
/// that live in a real source file.
pub fn index(a: &Assembled) -> SymbolIndex {
    let mut idx = SymbolIndex::default();
    let lines: Vec<&str> = a.source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if let Some(block_name) = block_start(trimmed) {
            i = scan_block(a, &lines, i, block_name, &mut idx);
            continue;
        }
        if let Some(loc) = loc_at(a, i) {
            if let Some((name, detail)) = global_decl(trimmed) {
                idx.globals.insert(name, Symbol { detail, loc });
            } else if let Some((name, detail)) = function_def(trimmed) {
                idx.functions.insert(name, Symbol { detail, loc });
            }
        }
        i += 1;
    }
    idx
}

fn loc_at(a: &Assembled, i: usize) -> Option<Loc> {
    a.map.get(i)?.clone()
}

/// The block name, if `trimmed` opens a UBO/interface block — e.g.
/// `layout(std140) uniform windUniforms {`.
fn block_start(trimmed: &str) -> Option<String> {
    if !trimmed.ends_with('{') {
        return None;
    }
    let rest = trimmed.strip_prefix("layout").map(str::trim_start).unwrap_or(trimmed);
    let rest = if let Some(after_paren) = rest.strip_prefix('(') {
        &after_paren[after_paren.find(')')? + 1..]
    } else {
        rest
    };
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("uniform")
        .or_else(|| rest.strip_prefix("buffer"))?
        .trim_start();
    let name: String = rest.chars().take_while(|c| is_word_char(*c)).collect();
    (!name.is_empty()).then_some(name)
}

/// Consume a block's members + its `} instance;` line, recording the UBO. Returns
/// the index of the first line after the block.
fn scan_block(a: &Assembled, lines: &[&str], start: usize, block_name: String, idx: &mut SymbolIndex) -> usize {
    let mut members = HashMap::new();
    let mut j = start + 1;
    while j < lines.len() && !lines[j].contains('}') {
        if let (Some((ty, name)), Some(loc)) = (member_decl(lines[j].trim()), loc_at(a, j)) {
            members.insert(name.clone(), Symbol { detail: format!("{ty} {name}"), loc });
        }
        j += 1;
    }
    if j < lines.len() {
        if let Some(instance) = block_end(lines[j].trim()) {
            if let Some(loc) = loc_at(a, start).or_else(|| loc_at(a, j)) {
                idx.ubos.insert(
                    instance.clone(),
                    Ubo {
                        detail: format!("uniform {block_name} {{ … }} {instance}"),
                        block_name,
                        loc,
                        members,
                    },
                );
            }
        }
        return j + 1;
    }
    start + 1
}

/// `float uMin;` -> `("float", "uMin")`. Only the simple `type name;` form.
fn member_decl(trimmed: &str) -> Option<(String, String)> {
    let body = trimmed.strip_suffix(';')?;
    let mut it = body.split_whitespace();
    let ty = it.next()?.to_string();
    let name = it.next()?.split('[').next()?.to_string();
    if it.next().is_some() || !is_ident(&ty) || !is_ident(&name) {
        return None;
    }
    Some((ty, name))
}

/// `} wind;` -> `"wind"`.
fn block_end(trimmed: &str) -> Option<String> {
    let body = trimmed.strip_prefix('}')?.trim_start().strip_suffix(';')?;
    let name = body.trim().split('[').next()?.trim().to_string();
    is_ident(&name).then_some(name)
}

/// `uniform sampler2D u_wind;` -> `("u_wind", "uniform sampler2D u_wind")`.
fn global_decl(trimmed: &str) -> Option<(String, String)> {
    let body = trimmed.strip_suffix(';')?;
    let first = body.split_whitespace().next()?;
    if !QUALIFIERS.contains(&first) {
        return None;
    }
    let lhs = body.split('=').next()?;
    let name = lhs.split_whitespace().last()?.split('[').next()?.to_string();
    is_ident(&name).then_some((name, body.trim().to_string()))
}

/// `vec2 windAt(sampler2D w, vec2 p) {` -> `("windAt", "vec2 windAt(sampler2D w, vec2 p)")`.
fn function_def(trimmed: &str) -> Option<(String, String)> {
    let head = trimmed.strip_suffix('{')?.trim_end();
    if !head.ends_with(')') {
        return None;
    }
    let before = &head[..head.find('(')?];
    let mut it = before.split_whitespace();
    let _ret = it.next()?;
    let name = it.next()?;
    if it.next().is_some() || !is_ident(name) {
        return None; // not a `<type> <name>(...)` definition
    }
    Some((name.to_string(), head.trim().to_string()))
}

/// Resolve the identifier at `col` (0-based) on `line` to a hover/definition hit.
pub fn resolve(index: &SymbolIndex, line: &str, col: usize) -> Option<Hit> {
    let (word, start) = word_at(line, col)?;
    let before = line[..start].trim_end();
    if let Some(prefix) = before.strip_suffix('.') {
        // `instance.member` — resolve the member within its block.
        let inst = trailing_ident(prefix.trim_end())?;
        let ubo = index.ubos.get(inst)?;
        let m = ubo.members.get(word)?;
        return Some(Hit {
            detail: m.detail.clone(),
            note: Some(format!("member of `{}`", ubo.block_name)),
            loc: m.loc.clone(),
        });
    }
    if let Some(s) = index.functions.get(word).or_else(|| index.globals.get(word)) {
        return Some(Hit { detail: s.detail.clone(), note: None, loc: s.loc.clone() });
    }
    index.ubos.get(word).map(|u| Hit {
        detail: u.detail.clone(),
        note: None,
        loc: u.loc.clone(),
    })
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && s.chars().all(is_word_char)
}

/// The identifier span covering `col`, with its start byte. `None` if `col` isn't
/// on a word. Columns are treated as byte offsets — correct for ASCII GLSL.
fn word_at(line: &str, col: usize) -> Option<(&str, usize)> {
    let b = line.as_bytes();
    let col = col.min(line.len());
    let mut start = col;
    while start > 0 && is_word_byte(b[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < line.len() && is_word_byte(b[end]) {
        end += 1;
    }
    (start != end).then(|| (&line[start..end], start))
}

/// The identifier at the very end of `s` (e.g. the instance before a `.`).
fn trailing_ident(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let end = s.len();
    if end == 0 || !is_word_byte(b[end - 1]) {
        return None;
    }
    let mut start = end;
    while start > 0 && is_word_byte(b[start - 1]) {
        start -= 1;
    }
    Some(&s[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assemble::{Assembled, Stage};
    use std::path::PathBuf;

    fn loc(file: &str, line: u32) -> Loc {
        Loc { path: PathBuf::from(file), line }
    }

    // Build an Assembled from (line, Loc) pairs — mimics windUniforms.glsl injected
    // into draw.vert.glsl.
    fn fixture() -> Assembled {
        let rows: &[(&str, Loc)] = &[
            ("layout(std140) uniform windUniforms {", loc("/p/windUniforms.glsl", 1)),
            ("  float uMin;", loc("/p/windUniforms.glsl", 2)),
            ("  float maxSpeed;", loc("/p/windUniforms.glsl", 3)),
            ("} wind;", loc("/p/windUniforms.glsl", 4)),
            ("uniform sampler2D u_wind;", loc("/p/draw.vert.glsl", 5)),
            ("vec2 windAt(sampler2D windTex, vec2 pos) {", loc("/p/draw.vert.glsl", 9)),
            ("  return vec2(0.0);", loc("/p/draw.vert.glsl", 10)),
            ("}", loc("/p/draw.vert.glsl", 11)),
        ];
        let source = rows.iter().map(|(l, _)| *l).collect::<Vec<_>>().join("\n") + "\n";
        let map = rows.iter().map(|(_, l)| Some(l.clone())).collect();
        Assembled { source, stage: Stage::Vertex, map, target: PathBuf::from("/p/draw.vert.glsl"), note: None }
    }

    #[test]
    fn indexes_ubo_block_with_members() {
        let idx = index(&fixture());
        let ubo = idx.ubos.get("wind").expect("wind UBO");
        assert_eq!(ubo.block_name, "windUniforms");
        assert_eq!(ubo.loc.line, 1); // block declaration
        let m = ubo.members.get("maxSpeed").expect("maxSpeed member");
        assert_eq!(m.detail, "float maxSpeed");
        assert_eq!(m.loc.path, PathBuf::from("/p/windUniforms.glsl"));
        assert_eq!(m.loc.line, 3);
    }

    #[test]
    fn indexes_globals_and_functions() {
        let idx = index(&fixture());
        assert_eq!(idx.globals.get("u_wind").unwrap().detail, "uniform sampler2D u_wind");
        assert_eq!(idx.globals.get("u_wind").unwrap().loc.line, 5);
        let f = idx.functions.get("windAt").expect("windAt fn");
        assert_eq!(f.loc.line, 9);
        assert!(f.detail.starts_with("vec2 windAt("));
        // The UBO members must not leak into globals.
        assert!(!idx.globals.contains_key("uMin"));
    }

    #[test]
    fn resolves_member_access_across_files() {
        let idx = index(&fixture());
        let line = "  v = clamp(x, wind.maxSpeed, 1.0);";
        let col = line.find("maxSpeed").unwrap() + 3; // mid-word
        let hit = resolve(&idx, line, col).expect("resolves wind.maxSpeed");
        assert_eq!(hit.detail, "float maxSpeed");
        assert_eq!(hit.loc.path, PathBuf::from("/p/windUniforms.glsl"));
        assert_eq!(hit.loc.line, 3);
        assert_eq!(hit.note.as_deref(), Some("member of `windUniforms`"));
    }

    #[test]
    fn resolves_function_and_global_and_misses_gracefully() {
        let idx = index(&fixture());
        assert_eq!(resolve(&idx, "y = windAt(u_wind, p);", 4).unwrap().loc.line, 9);
        let g = resolve(&idx, "z = texture(u_wind, p);", "z = texture(".len() + 1).unwrap();
        assert_eq!(g.loc.line, 5); // u_wind global
        // Cursor on whitespace, and an unknown member, both resolve to nothing.
        assert!(resolve(&idx, "  a + b", 3).is_none());
        assert!(resolve(&idx, "wind.nope", 6).is_none());
    }
}
