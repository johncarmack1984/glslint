//! A lightweight symbol scanner over the assembled unit, powering hover and
//! go-to-definition. Deliberately line-based (like the assembler and the lints),
//! not a full GLSL parser — it recognizes the shapes luma/deck shaders actually
//! use: UBO/interface blocks, top-level `uniform`/`in`/`out` declarations, and
//! function definitions. Every symbol carries its original `Loc` via the line
//! map, so a `wind.uMin` access in one file resolves to its declaration in the
//! injected `windUniforms.glsl` — the cross-module jump stock tools can't do.

use crate::assemble::{Assembled, Loc};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Symbol {
    pub detail: String,
    pub loc: Loc,
}

/// A built-in function — deck.gl prelude or core GLSL. Hover/completion only.
#[derive(Debug, Clone)]
pub struct Builtin {
    pub signature: String,
    pub origin: &'static str,
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
    /// Built-in functions (deck prelude + core GLSL) keyed by name — hover/
    /// completion only; no source location to navigate to.
    pub builtins: HashMap<String, Builtin>,
}

/// A resolved hover / definition target. `loc` is `None` for deck builtins, which
/// can be hovered but have no source location to navigate to.
#[derive(Debug, Clone)]
pub struct Hit {
    pub detail: String,
    pub note: Option<String>,
    pub loc: Option<Loc>,
}

/// The flavor of a symbol, mapped to LSP completion/symbol kinds in `lsp.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymKind {
    Field,
    Function,
    Variable,
    Builtin,
    Block,
}

/// A completion candidate.
#[derive(Debug, Clone)]
pub struct Completion {
    pub label: String,
    pub detail: String,
    pub kind: SymKind,
}

/// A document-outline entry (1-based `line` into the open document).
#[derive(Debug, Clone)]
pub struct DocSym {
    pub name: String,
    pub detail: String,
    pub kind: SymKind,
    pub line: u32,
    pub children: Vec<DocSym>,
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
    index_builtins(&mut idx);
    index_glsl_builtins(&mut idx);
    idx
}

/// Index the deck.gl prelude's functions as hover-only builtins.
fn index_builtins(idx: &mut SymbolIndex) {
    for line in crate::assemble::BUILTIN_PRELUDE.lines() {
        if let Some((name, sig)) = builtin_signature(line.trim()) {
            idx.builtins
                .entry(name)
                .or_insert(Builtin { signature: sig, origin: "deck.gl built-in (injected at link time)" });
        }
    }
}

/// Index the core GLSL ES built-in functions (the ones with no source file).
/// Signatures use the spec's shorthand (`genType`, `gvec4`, `sampler`).
fn index_glsl_builtins(idx: &mut SymbolIndex) {
    for (name, sig) in GLSL_BUILTINS {
        idx.builtins
            .entry((*name).to_string())
            .or_insert(Builtin { signature: (*sig).to_string(), origin: "GLSL ES built-in" });
    }
}

#[rustfmt::skip]
const GLSL_BUILTINS: &[(&str, &str)] = &[
    ("radians", "genType radians(genType degrees)"),
    ("degrees", "genType degrees(genType radians)"),
    ("sin", "genType sin(genType angle)"), ("cos", "genType cos(genType angle)"),
    ("tan", "genType tan(genType angle)"), ("asin", "genType asin(genType x)"),
    ("acos", "genType acos(genType x)"), ("atan", "genType atan(genType y, genType x)"),
    ("pow", "genType pow(genType x, genType y)"), ("exp", "genType exp(genType x)"),
    ("log", "genType log(genType x)"), ("exp2", "genType exp2(genType x)"),
    ("log2", "genType log2(genType x)"), ("sqrt", "genType sqrt(genType x)"),
    ("inversesqrt", "genType inversesqrt(genType x)"), ("abs", "genType abs(genType x)"),
    ("sign", "genType sign(genType x)"), ("floor", "genType floor(genType x)"),
    ("ceil", "genType ceil(genType x)"), ("fract", "genType fract(genType x)"),
    ("mod", "genType mod(genType x, genType y)"), ("round", "genType round(genType x)"),
    ("min", "genType min(genType x, genType y)"), ("max", "genType max(genType x, genType y)"),
    ("clamp", "genType clamp(genType x, genType minVal, genType maxVal)"),
    ("mix", "genType mix(genType x, genType y, genType a)"),
    ("step", "genType step(genType edge, genType x)"),
    ("smoothstep", "genType smoothstep(genType edge0, genType edge1, genType x)"),
    ("length", "float length(genType x)"), ("distance", "float distance(genType p0, genType p1)"),
    ("dot", "float dot(genType x, genType y)"), ("cross", "vec3 cross(vec3 x, vec3 y)"),
    ("normalize", "genType normalize(genType x)"),
    ("reflect", "genType reflect(genType I, genType N)"),
    ("refract", "genType refract(genType I, genType N, float eta)"),
    ("texture", "gvec4 texture(sampler tex, vec coord [, float bias])"),
    ("textureLod", "gvec4 textureLod(sampler tex, vec coord, float lod)"),
    ("texelFetch", "gvec4 texelFetch(sampler tex, ivec coord, int lod)"),
    ("textureSize", "ivec textureSize(sampler tex, int lod)"),
    ("dFdx", "genType dFdx(genType p)"), ("dFdy", "genType dFdy(genType p)"),
    ("fwidth", "genType fwidth(genType p)"),
    ("transpose", "mat transpose(mat m)"), ("inverse", "mat inverse(mat m)"),
];

/// Extract `(name, signature)` from a possibly single-line-body function like
/// `vec2 project_pixel_size_to_clipspace(vec2 pixels) { return pixels; }`.
fn builtin_signature(line: &str) -> Option<(String, String)> {
    if !line.contains('{') {
        return None; // a body / closing line, not a definition
    }
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let mut it = line[..open].split_whitespace();
    let _ret = it.next()?;
    let name = it.next()?;
    if it.next().is_some() || !is_ident(name) {
        return None;
    }
    Some((name.to_string(), line[..=close].trim().to_string()))
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
            loc: Some(m.loc.clone()),
        });
    }
    if let Some(s) = index.functions.get(word).or_else(|| index.globals.get(word)) {
        return Some(Hit { detail: s.detail.clone(), note: None, loc: Some(s.loc.clone()) });
    }
    if let Some(u) = index.ubos.get(word) {
        return Some(Hit { detail: u.detail.clone(), note: None, loc: Some(u.loc.clone()) });
    }
    index.builtins.get(word).map(|b| Hit {
        detail: b.signature.clone(),
        note: Some(b.origin.to_string()),
        loc: None,
    })
}

/// Completion candidates for the cursor at `col` (0-based) on `line`. After
/// `<instance>.` it offers that block's members; otherwise the visible
/// functions, globals, block instances, and deck builtins.
pub fn complete(index: &SymbolIndex, line: &str, col: usize) -> Vec<Completion> {
    let prefix = &line[..col.min(line.len())];
    if let Some(inst) = member_context(prefix) {
        // `instance.` — members only, and nothing if the instance is unknown
        // (we won't pollute a non-UBO member access with globals).
        return match index.ubos.get(inst) {
            Some(ubo) => ubo
                .members
                .iter()
                .map(|(name, s)| Completion { label: name.clone(), detail: s.detail.clone(), kind: SymKind::Field })
                .collect(),
            None => Vec::new(),
        };
    }
    let mut out = Vec::new();
    for (n, s) in &index.functions {
        out.push(Completion { label: n.clone(), detail: s.detail.clone(), kind: SymKind::Function });
    }
    for (n, s) in &index.globals {
        out.push(Completion { label: n.clone(), detail: s.detail.clone(), kind: SymKind::Variable });
    }
    for (n, u) in &index.ubos {
        out.push(Completion { label: n.clone(), detail: u.detail.clone(), kind: SymKind::Block });
    }
    for (n, b) in &index.builtins {
        out.push(Completion { label: n.clone(), detail: b.signature.clone(), kind: SymKind::Builtin });
    }
    out
}

/// If `prefix` ends with `<instance>.<partial>`, return the instance name.
fn member_context(prefix: &str) -> Option<&str> {
    let b = prefix.as_bytes();
    let mut end = prefix.len();
    while end > 0 && is_word_byte(b[end - 1]) {
        end -= 1; // skip the partial member being typed
    }
    let head = prefix[..end].strip_suffix('.')?;
    trailing_ident(head)
}

/// Outline of the symbols *declared in* `file` (so opening a stage shader lists
/// its own globals/functions, and opening a `*Uniforms.glsl` lists the block).
pub fn document_symbols(index: &SymbolIndex, file: &Path) -> Vec<DocSym> {
    let here = |loc: &Loc| same_path(&loc.path, file);
    let mut out = Vec::new();
    for (name, u) in &index.ubos {
        if !here(&u.loc) {
            continue;
        }
        let mut members: Vec<DocSym> = u
            .members
            .iter()
            .filter(|(_, m)| here(&m.loc))
            .map(|(mn, m)| DocSym { name: mn.clone(), detail: m.detail.clone(), kind: SymKind::Field, line: m.loc.line, children: Vec::new() })
            .collect();
        members.sort_by_key(|d| d.line);
        out.push(DocSym { name: name.clone(), detail: format!("uniform {}", u.block_name), kind: SymKind::Block, line: u.loc.line, children: members });
    }
    for (name, s) in &index.functions {
        if here(&s.loc) {
            out.push(DocSym { name: name.clone(), detail: s.detail.clone(), kind: SymKind::Function, line: s.loc.line, children: Vec::new() });
        }
    }
    for (name, s) in &index.globals {
        if here(&s.loc) {
            out.push(DocSym { name: name.clone(), detail: s.detail.clone(), kind: SymKind::Variable, line: s.loc.line, children: Vec::new() });
        }
    }
    out.sort_by_key(|d| d.line);
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
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
        let loc = hit.loc.as_ref().unwrap();
        assert_eq!(loc.path, PathBuf::from("/p/windUniforms.glsl"));
        assert_eq!(loc.line, 3);
        assert_eq!(hit.note.as_deref(), Some("member of `windUniforms`"));
    }

    #[test]
    fn resolves_function_and_global_and_misses_gracefully() {
        let idx = index(&fixture());
        assert_eq!(resolve(&idx, "y = windAt(u_wind, p);", 4).unwrap().loc.unwrap().line, 9);
        let g = resolve(&idx, "z = texture(u_wind, p);", "z = texture(".len() + 1).unwrap();
        assert_eq!(g.loc.unwrap().line, 5); // u_wind global
        // Cursor on whitespace, and an unknown member, both resolve to nothing.
        assert!(resolve(&idx, "  a + b", 3).is_none());
        assert!(resolve(&idx, "wind.nope", 6).is_none());
    }

    #[test]
    fn deck_builtin_is_hover_only() {
        let idx = index(&fixture());
        let line = "  vec4 c = project_position_to_clipspace(a, b, d);";
        let col = line.find("project_position_to_clipspace").unwrap() + 5;
        let hit = resolve(&idx, line, col).expect("builtin resolves");
        assert!(hit.detail.starts_with("vec4 project_position_to_clipspace("));
        assert!(hit.loc.is_none()); // no source file -> no go-to-definition
        assert!(hit.note.as_deref().unwrap().contains("deck.gl"));
    }

    #[test]
    fn core_glsl_builtin_is_hover_only() {
        let idx = index(&fixture());
        let line = "  x = clamp(a, b, c);";
        let hit = resolve(&idx, line, line.find("clamp").unwrap() + 2).expect("clamp resolves");
        assert!(hit.detail.contains("clamp("));
        assert!(hit.loc.is_none());
        assert_eq!(hit.note.as_deref(), Some("GLSL ES built-in"));
        // It's offered in general completion too.
        assert!(complete(&idx, "  ", 2).iter().any(|c| c.label == "clamp"));
    }

    #[test]
    fn completes_members_after_dot() {
        let idx = index(&fixture());
        let labels: Vec<_> = complete(&idx, "  x = wind.", "  x = wind.".len())
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(labels.contains(&"uMin".to_string()));
        assert!(labels.contains(&"maxSpeed".to_string()));
        // Only members — no globals/functions leak into a member completion.
        assert!(!labels.contains(&"windAt".to_string()));
        // A partial member still completes from the instance.
        assert!(complete(&idx, "x = wind.max", "x = wind.max".len()).iter().any(|c| c.label == "maxSpeed"));
        // Unknown instance -> nothing (don't guess).
        assert!(complete(&idx, "foo.", "foo.".len()).is_empty());
    }

    #[test]
    fn general_completion_lists_visible_symbols() {
        let idx = index(&fixture());
        let labels: Vec<_> = complete(&idx, "  ", 2).into_iter().map(|c| c.label).collect();
        for want in ["windAt", "u_wind", "wind", "project_position"] {
            assert!(labels.contains(&want.to_string()), "missing {want}");
        }
    }

    #[test]
    fn document_symbols_are_scoped_to_the_file() {
        let idx = index(&fixture());
        let draw = document_symbols(&idx, Path::new("/p/draw.vert.glsl"));
        let names: Vec<_> = draw.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"windAt") && names.contains(&"u_wind"));
        // The wind block lives in windUniforms.glsl, not here.
        assert!(!names.contains(&"wind"));
        // Outline is ordered by line.
        assert!(draw.windows(2).all(|w| w[0].line <= w[1].line));

        let module = document_symbols(&idx, Path::new("/p/windUniforms.glsl"));
        let wind = module.iter().find(|d| d.name == "wind").expect("wind block");
        assert!(wind.children.iter().any(|c| c.name == "maxSpeed"));
    }
}
