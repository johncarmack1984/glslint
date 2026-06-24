//! Auto-derive shader -> module bindings from the JS/TS `new Model({ modules })`
//! calls, so a project needs no `glsl-lsp.toml`. This is a heuristic scan (not a
//! JS parser), matching the conventional deck/luma shape:
//!
//! ```js
//! import DRAW_VS from './shaders/draw.vert.glsl?raw';
//! import { project32 } from '@deck.gl/core';
//! import { windUniforms } from './modules';
//! new Model(device, { vs: DRAW_VS, fs: DRAW_FS, modules: [project32, windUniforms] });
//! ```
//!
//! Each module is resolved to its GLSL source (a local module's `vs:` import) or
//! to the deck builtin prelude (a module imported from a package like @deck.gl).
//! Conservative — returns `None` when it can't confidently resolve, so the caller
//! falls back to explicit config or sibling discovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Derived {
    pub modules: Vec<PathBuf>,
    pub use_builtin_prelude: bool,
}

/// Derive `shader`'s modules by finding the `new Model(...)` call that wires it up.
pub fn derive(shader: &Path) -> Option<Derived> {
    let name = shader.file_name()?.to_str()?;
    for ts in candidate_ts(shader) {
        if let Ok(text) = std::fs::read_to_string(&ts) {
            if let Some(d) = derive_from(&text, &ts, name) {
                return Some(d);
            }
        }
    }
    None
}

fn derive_from(text: &str, ts: &Path, shader_name: &str) -> Option<Derived> {
    let imports = es_imports(text);
    // local binding for this shader file, e.g. `DRAW_VS`
    let binding = imports
        .iter()
        .find(|(_, p)| strip_query(p).ends_with(shader_name))
        .map(|(n, _)| n.clone())?;
    let module_ids = model_modules_for(text, &binding)?;

    let ts_dir = ts.parent()?;
    let mut modules = Vec::new();
    let mut use_builtin_prelude = false;
    for id in module_ids {
        match imports.get(&id) {
            // a local module (./modules) -> follow it to its GLSL source
            Some(p) if is_relative(p) => {
                if let Some(glsl) = resolve_local_module(ts_dir, p, &id) {
                    modules.push(glsl);
                }
            }
            // a package module (@deck.gl/@luma, e.g. project32) -> the deck builtins
            Some(_) => use_builtin_prelude = true,
            None => {}
        }
    }
    // Resolved nothing usable — let the caller fall back to sibling discovery.
    if modules.is_empty() && !use_builtin_prelude {
        return None;
    }
    Some(Derived { modules, use_builtin_prelude })
}

/// `*.ts`/`*.js` files in the shader's ancestor directories, up to the project
/// root (a dir with `package.json`/`.git`).
fn candidate_ts(shader: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dir = shader.parent();
    let mut levels = 0;
    while let Some(d) = dir {
        if let Ok(entries) = std::fs::read_dir(d) {
            for e in entries.flatten() {
                let p = e.path();
                if matches!(p.extension().and_then(|x| x.to_str()), Some("ts" | "tsx" | "js" | "jsx" | "mts"))
                    && !p.to_string_lossy().contains("node_modules")
                {
                    out.push(p);
                }
            }
        }
        levels += 1;
        if d.join("package.json").exists() || d.join(".git").exists() || levels > 6 {
            break;
        }
        dir = d.parent();
    }
    out
}

/// Map of local name -> import specifier. Anchored on `from '<path>'` (looking
/// back to the preceding `import`), so it handles default, named, and *multi-line*
/// `import { … } from '…'` forms.
fn es_imports(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut search = 0;
    while let Some(rel) = text[search..].find(" from ") {
        let fpos = search + rel;
        search = fpos + 6;
        let Some(path) = first_quoted(&text[fpos + 6..]) else { continue };
        if let Some(ipos) = text[..fpos].rfind("import ") {
            let names = &text[ipos + 7..fpos];
            // a `;` between `import` and `from` means they're separate statements
            if !names.contains(';') {
                add_import_names(&mut map, names, &path);
            }
        }
    }
    map
}

fn add_import_names(map: &mut HashMap<String, String>, names: &str, path: &str) {
    let names = names.trim();
    if let Some(open) = names.find('{') {
        let default = names[..open].trim().trim_end_matches(',').trim();
        if is_ident(default) {
            map.insert(default.to_string(), path.to_string());
        }
        if let Some(close) = names.rfind('}') {
            for part in names[open + 1..close].split(',') {
                let local = part.trim().trim_start_matches("type ").split(" as ").last().unwrap_or("").trim();
                if is_ident(local) {
                    map.insert(local.to_string(), path.to_string());
                }
            }
        }
    } else if is_ident(names) {
        map.insert(names.to_string(), path.to_string());
    }
}

/// The contents of the first `'…'` or `"…"` string in `s`.
fn first_quoted(s: &str) -> Option<String> {
    let s = s.trim_start();
    let q = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = &s[q.len_utf8()..];
    rest.find(q).map(|end| rest[..end].to_string())
}

/// The module identifiers in the `modules: [...]` of the `new Model(...)` call
/// that references `binding` (the shader's `vs`/`fs`).
fn model_modules_for(text: &str, binding: &str) -> Option<Vec<String>> {
    let mut search = 0;
    while let Some(rel) = text[search..].find("new Model") {
        let start = search + rel;
        let paren = text[start..].find('(').map(|o| start + o)?;
        let (call, end) = bracket_span(text, paren, b'(', b')')?;
        search = end;
        if !contains_ident(&call, binding) {
            continue;
        }
        if let Some(mi) = call.find("modules") {
            if let Some(lb) = call[mi..].find('[').map(|o| mi + o) {
                if let Some((arr, _)) = bracket_span(&call, lb, b'[', b']') {
                    let ids: Vec<String> =
                        arr.split(',').map(|s| s.trim().to_string()).filter(|s| is_ident(s)).collect();
                    if !ids.is_empty() {
                        return Some(ids);
                    }
                }
            }
        }
    }
    None
}

/// Resolve a local module id (e.g. `windUniforms` from `./modules`) to the GLSL
/// file behind its `vs:` field.
fn resolve_local_module(ts_dir: &Path, import_path: &str, id: &str) -> Option<PathBuf> {
    let module_file = resolve_ts(&ts_dir.join(strip_query(import_path)))?;
    let text = std::fs::read_to_string(&module_file).ok()?;
    let glsl_var = module_glsl_var(&text, id)?;
    let glsl_path = es_imports(&text).get(&glsl_var)?.clone();
    Some(module_file.parent()?.join(strip_query(&glsl_path)))
}

/// The identifier assigned to `<id>`'s `vs:` field in a `const <id> = { … }`.
fn module_glsl_var(text: &str, id: &str) -> Option<String> {
    let decl = find_const(text, id)?;
    let brace = text[decl..].find('{').map(|o| decl + o)?;
    let (obj, _) = bracket_span(text, brace, b'{', b'}')?;
    // Fields may be one-per-line or comma-separated on one line.
    for field in obj.split([',', '\n']) {
        let t = field.trim();
        if let Some(rest) = t.strip_prefix("vs:").or_else(|| t.strip_prefix("vs :")) {
            let v = rest.trim().trim_end_matches(',').trim();
            if is_ident(v) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn resolve_ts(base: &Path) -> Option<PathBuf> {
    for ext in ["ts", "tsx", "mts", "js", "jsx"] {
        let p = base.with_extension(ext);
        if p.is_file() {
            return Some(p);
        }
    }
    for idx in ["index.ts", "index.tsx", "index.js"] {
        let p = base.join(idx);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Byte offset just past a `const <id>` (optionally `export const`) declaration.
fn find_const(text: &str, id: &str) -> Option<usize> {
    let needle = format!("const {id}");
    let mut from = 0;
    while let Some(rel) = text[from..].find(&needle) {
        let start = from + rel;
        let after = start + needle.len();
        if text[after..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_') {
            return Some(after);
        }
        from = start + 1;
    }
    None
}

/// Content between a leading bracket at `open` and its match (handles nesting).
fn bracket_span(text: &str, open: usize, ob: u8, cb: u8) -> Option<(String, usize)> {
    let b = text.as_bytes();
    if b.get(open) != Some(&ob) {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        if b[i] == ob {
            depth += 1;
        } else if b[i] == cb {
            depth -= 1;
            if depth == 0 {
                return Some((text[open + 1..i].to_string(), i + 1));
            }
        }
        i += 1;
    }
    None
}

fn strip_query(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

fn is_relative(path: &str) -> bool {
    path.starts_with('.') || path.starts_with('/')
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `ident` appears as a whole word in `text`.
fn contains_ident(text: &str, ident: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(ident) {
        let s = from + rel;
        let e = s + ident.len();
        let lhs = s == 0 || !is_word(b[s - 1]);
        let rhs = e == b.len() || !is_word(b[e]);
        if lhs && rhs {
            return true;
        }
        from = s + 1;
    }
    false
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    // project32 in a *multi-line* import, like the real WindLayer.ts.
    const WINDLAYER: &str = "import {\n  CompositeLayer,\n  project32,\n} from '@deck.gl/core';\n\
        import { blitUniforms, windUniforms } from './modules';\n\
        import DRAW_VS from './shaders/draw.vert.glsl?raw';\n\
        import DRAW_FS from './shaders/draw.frag.glsl?raw';\n\
        import BLIT_VS from './shaders/blit.vert.glsl?raw';\n\
        const model = new Model(device, {\n\
          vs: DRAW_VS,\n\
          fs: DRAW_FS,\n\
          modules: [project32, windUniforms],\n\
        });\n\
        new Model(device, { vs: BLIT_VS, modules: [blitUniforms] });\n";

    #[test]
    fn parses_imports_default_and_named() {
        let m = es_imports(WINDLAYER);
        assert_eq!(m["DRAW_VS"], "./shaders/draw.vert.glsl?raw");
        assert_eq!(m["windUniforms"], "./modules");
        assert_eq!(m["project32"], "@deck.gl/core");
    }

    #[test]
    fn finds_modules_for_a_shader_binding() {
        assert_eq!(model_modules_for(WINDLAYER, "DRAW_VS").unwrap(), vec!["project32", "windUniforms"]);
        assert_eq!(model_modules_for(WINDLAYER, "DRAW_FS").unwrap(), vec!["project32", "windUniforms"]);
        assert_eq!(model_modules_for(WINDLAYER, "BLIT_VS").unwrap(), vec!["blitUniforms"]);
    }

    #[test]
    fn extracts_a_modules_vs_glsl_var() {
        let modules = "import WIND from './shaders/windUniforms.glsl?raw';\n\
            export const windUniforms: any = { name: 'wind', vs: WIND, fs: WIND, uniformTypes: { a: 'f32' } };\n";
        assert_eq!(module_glsl_var(modules, "windUniforms").unwrap(), "WIND");
        assert_eq!(es_imports(modules)["WIND"], "./shaders/windUniforms.glsl?raw");
    }

    #[test]
    fn contains_ident_is_whole_word() {
        assert!(contains_ident("vs: DRAW_VS,", "DRAW_VS"));
        assert!(!contains_ident("vs: DRAW_VS_EXTRA,", "DRAW_VS"));
    }
}
