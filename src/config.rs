//! Per-file configuration: which preludes and luma/deck module fragments to
//! splice in before validating. Resolved from a `glsl-lsp.toml` found by walking
//! up from the target file, or sensible zero-config defaults.
//!
//! The richest form mirrors luma's own model: name the modules once, then bind
//! each shader to the modules it actually uses (the `new Model({modules: [...]})`
//! call in JS). Each shader then gets exactly its modules, not every sibling:
//!
//! ```toml
//! [[module]]
//! name = "windUniforms"
//! source = "src/shaders/windUniforms.glsl"
//!
//! [[module]]
//! name = "project32"
//! builtin = true          # deck project32 (baked-in stub for now)
//!
//! [[shader]]
//! match   = "draw.*.glsl"  # first matching binding wins
//! modules = ["project32", "windUniforms"]
//! ```
//!
//! Without `[[shader]]` bindings it falls back to a legacy global `modules` list,
//! and with no `glsl-lsp.toml` at all to zero-config sibling discovery.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigFile {
    /// Extra GLSL files prepended verbatim (after `#version`, before modules).
    #[serde(default)]
    preludes: Vec<String>,
    /// Legacy global module list (applied to every shader). Superseded by
    /// `[[module]]` + `[[shader]]` bindings when those are present.
    #[serde(default)]
    modules: Vec<String>,
    /// Legacy global toggle for the baked-in deck prelude.
    builtin_prelude: Option<bool>,
    /// Named module definitions (`[[module]]`).
    #[serde(default, rename = "module")]
    module_defs: Vec<ModuleDef>,
    /// Per-shader module bindings (`[[shader]]`).
    #[serde(default, rename = "shader")]
    shaders: Vec<ShaderBinding>,
    /// Shader-dialect selection and custom directive rules (`[dialect]`).
    #[serde(default)]
    dialect: DialectFile,
}

/// The `[dialect]` table: pick a built-in preset, toggle auto-detection, and/or
/// extend it with project-local rules and prelude.
#[derive(Debug, Clone, Deserialize, Default)]
struct DialectFile {
    /// A built-in preset name (`"maplibre"`, `"shadertoy"`, `"none"`). When set,
    /// it replaces auto-detection as the base dialect.
    preset: Option<String>,
    /// Whether to sniff the source for a dialect when no `preset` is given.
    /// Defaults to on, so a maplibre checkout works with zero config.
    auto: Option<bool>,
    /// Extra prelude prepended for every stage (implicit globals).
    prelude: Option<String>,
    /// Project-local expansion rules (`[[dialect.expand]]`), merged ahead of the
    /// base dialect's own so a project can override a preset rule.
    #[serde(default, rename = "expand")]
    expand: Vec<ExpandRuleFile>,
}

/// One `[[dialect.expand]]` rule in `glsl-lsp.toml`.
#[derive(Debug, Clone, Deserialize)]
struct ExpandRuleFile {
    /// Pragma namespace: matches `#pragma <pragma>: …`.
    pragma: String,
    /// First token after the namespace to match, or omit to match any.
    verb: Option<String>,
    /// Names bound, in order, to the remaining whitespace tokens.
    #[serde(default)]
    args: Vec<String>,
    /// Expansion template; `{arg}` placeholders are substituted. Applies to every
    /// stage unless a stage-specific field overrides it.
    emit: Option<String>,
    /// Vertex-stage override for `emit`.
    vertex: Option<String>,
    /// Fragment-stage override for `emit`.
    fragment: Option<String>,
    /// Compute-stage override for `emit`.
    compute: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModuleDef {
    name: String,
    /// A `.glsl` file providing the module's declarations.
    source: Option<String>,
    /// The baked-in deck project32 prelude stub.
    #[serde(default)]
    builtin: bool,
    /// A JS/TS file whose `uniformTypes` mirrors this module's UBO block. When
    /// set, glslint cross-checks the two and warns on drift.
    types: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ShaderBinding {
    /// Glob matched against the shader's filename (or path relative to the toml
    /// when the pattern contains `/`).
    #[serde(rename = "match")]
    pattern: String,
    /// Module names (from `[[module]]`) this shader uses.
    #[serde(default)]
    modules: Vec<String>,
    /// Override the deck prelude for this binding (else inferred from a bound
    /// `builtin` module).
    builtin_prelude: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub preludes: Vec<PathBuf>,
    pub modules: Vec<PathBuf>,
    pub use_builtin_prelude: bool,
    /// Which shader dialect (maplibre, shadertoy, …) and custom directive rules
    /// apply. Resolved to a concrete dialect at assembly time, once the source is
    /// known (auto-detection needs it).
    pub dialect: crate::dialect::Preference,
}

/// Build a dialect [`Preference`](crate::dialect::Preference) from the `[dialect]`
/// table, translating each `[[dialect.expand]]` entry into a rule.
fn dialect_pref(df: &DialectFile) -> crate::dialect::Preference {
    let custom_rules = df
        .expand
        .iter()
        .map(|e| crate::dialect::Rule {
            ns: e.pragma.clone(),
            verb: e.verb.clone(),
            args: e.args.clone(),
            vertex: e.vertex.clone(),
            fragment: e.fragment.clone(),
            compute: e.compute.clone(),
            any: e.emit.clone(),
        })
        .collect();
    crate::dialect::Preference {
        preset: df.preset.clone(),
        auto: df.auto.unwrap_or(true),
        custom_rules,
        custom_prelude: df.prelude.clone(),
    }
}

impl Config {
    /// Resolve config for `file`. Infallible: any IO/parse error degrades to the
    /// zero-config default rather than failing the check.
    pub fn resolve_for(file: &Path) -> Config {
        let dir = file.parent().unwrap_or(Path::new("."));

        // Read the project config (glslint.toml, else legacy glsl-lsp.toml).
        let found = find_config(dir).and_then(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let cf: ConfigFile = toml::from_str(&text).ok()?;
            let base = p.parent().unwrap_or(Path::new(".")).to_path_buf();
            Some((cf, base))
        });
        // The dialect preference comes from the file's `[dialect]` (or default) and
        // applies on every path below — even when the file has no module wiring.
        let dialect = found
            .as_ref()
            .map(|(cf, _)| dialect_pref(&cf.dialect))
            .unwrap_or_default();

        if let Some((cf, base)) = &found {
            // Per-shader bindings take precedence over the legacy global list.
            if !cf.shaders.is_empty() {
                return resolve_bindings(file, cf, base);
            }
            // Explicit module wiring (or an explicit deck toggle) wins. A file with
            // only a preset definition falls through to derivation/zero-config, so a
            // `glslint.toml` can hold just a preset without disabling luma discovery.
            if !cf.modules.is_empty() || !cf.preludes.is_empty() || cf.builtin_prelude.is_some() {
                return Config {
                    preludes: join_all(base, &cf.preludes),
                    modules: join_all(base, &cf.modules),
                    use_builtin_prelude: cf.builtin_prelude.unwrap_or(true),
                    dialect,
                };
            }
        }

        // No explicit module wiring — auto-derive luma bindings from the JS
        // `new Model({ modules })` calls before falling back to zero-config.
        if let Some(d) = crate::derive::derive(file) {
            return Config {
                preludes: Vec::new(),
                modules: d.modules,
                use_builtin_prelude: d.use_builtin_prelude,
                dialect,
            };
        }

        // Zero-config: the builtin deck/luma prelude, and no explicit modules —
        // `assemble` discovers the shared library (any no-`main` `.glsl` sibling,
        // which subsumes the old `*Uniforms.glsl` heuristic) and preset detection
        // stays on, so a maplibre checkout with no config still validates.
        Config {
            preludes: Vec::new(),
            modules: Vec::new(),
            use_builtin_prelude: true,
            dialect,
        }
    }
}

/// Resolve the modules for `file` from the first `[[shader]]` binding whose glob
/// matches it. An unmatched shader gets only the builtin prelude (it's likely a
/// module fragment or not yet bound).
fn resolve_bindings(file: &Path, cf: &ConfigFile, base: &Path) -> Config {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let rel = file
        .strip_prefix(base)
        .ok()
        .and_then(|r| r.to_str())
        .map(|s| s.replace('\\', "/"));
    let matches =
        |pat: &str| glob_match(pat, name) || rel.as_deref().is_some_and(|r| glob_match(pat, r));

    let Some(binding) = cf.shaders.iter().find(|s| matches(&s.pattern)) else {
        return Config {
            preludes: join_all(base, &cf.preludes),
            modules: Vec::new(),
            use_builtin_prelude: cf.builtin_prelude.unwrap_or(true),
            dialect: dialect_pref(&cf.dialect),
        };
    };

    let mut modules = Vec::new();
    let mut wants_builtin = false;
    for module_name in &binding.modules {
        // An unknown module name is ignored (could warn once we have a channel).
        if let Some(def) = cf.module_defs.iter().find(|m| &m.name == module_name) {
            if let Some(src) = &def.source {
                modules.push(base.join(src));
            }
            wants_builtin |= def.builtin;
        }
    }

    Config {
        preludes: join_all(base, &cf.preludes),
        modules,
        use_builtin_prelude: binding.builtin_prelude.unwrap_or(wants_builtin),
        dialect: dialect_pref(&cf.dialect),
    }
}

fn join_all(base: &Path, rels: &[String]) -> Vec<PathBuf> {
    rels.iter().map(|p| base.join(p)).collect()
}

/// If `file` is a module's GLSL `source` and that module declares a `types` JS
/// file, return `(module name, resolved types path)` for the drift check.
pub fn drift_for(file: &Path) -> Option<(String, PathBuf)> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let toml_path = find_config(dir)?;
    let text = std::fs::read_to_string(&toml_path).ok()?;
    let cf: ConfigFile = toml::from_str(&text).ok()?;
    let base = toml_path.parent().unwrap_or(Path::new("."));
    for m in &cf.module_defs {
        if let (Some(src), Some(types)) = (&m.source, &m.types)
            && same_path(&base.join(src), file)
        {
            return Some((m.name.clone(), base.join(types)));
        }
    }
    None
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Match `text` against a simple glob: `*` is any run (including `/`), `?` is any
/// single char. Backtracking two-pointer; no character classes.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            resume = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn find_up(start: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // Stop climbing at a repo root.
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    None
}

/// The project's config file: prefer `glslint.toml` (the unified name, shared with
/// presets), falling back to the legacy `glsl-lsp.toml`. Both hold the same module
/// / shader / prelude config; `glslint.toml` may also carry a preset definition,
/// which `preset.rs` reads independently (the two parsers each take their part).
fn find_config(start: &Path) -> Option<PathBuf> {
    find_up(start, "glslint.toml").or_else(|| find_up(start, "glsl-lsp.toml"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: unwrap IS the assertion
mod tests {
    use super::*;

    #[test]
    fn find_config_prefers_glslint_toml_over_legacy() {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "glslint-cfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Only the legacy name present → found.
        std::fs::write(dir.join("glsl-lsp.toml"), "").unwrap();
        assert!(find_config(&dir).unwrap().ends_with("glsl-lsp.toml"));
        // Both present → the unified name wins.
        std::fs::write(dir.join("glslint.toml"), "").unwrap();
        assert!(find_config(&dir).unwrap().ends_with("glslint.toml"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_matches_filename_patterns() {
        assert!(glob_match("draw.*.glsl", "draw.vert.glsl"));
        assert!(glob_match("draw.*.glsl", "draw.frag.glsl"));
        assert!(glob_match("*.frag.glsl", "blit.frag.glsl"));
        assert!(glob_match("blit.vert.glsl", "blit.vert.glsl"));
        assert!(!glob_match("draw.*.glsl", "blit.vert.glsl"));
        assert!(!glob_match("draw.*.glsl", "draw.vert.glsl.bak"));
        assert!(glob_match(
            "src/shaders/draw.*",
            "src/shaders/draw.vert.glsl"
        ));
    }

    fn cf() -> ConfigFile {
        ConfigFile {
            module_defs: vec![
                ModuleDef {
                    name: "windUniforms".into(),
                    source: Some("src/shaders/windUniforms.glsl".into()),
                    builtin: false,
                    types: None,
                },
                ModuleDef {
                    name: "blitUniforms".into(),
                    source: Some("src/shaders/blitUniforms.glsl".into()),
                    builtin: false,
                    types: None,
                },
                ModuleDef {
                    name: "project32".into(),
                    source: None,
                    builtin: true,
                    types: None,
                },
            ],
            shaders: vec![
                ShaderBinding {
                    pattern: "draw.*.glsl".into(),
                    modules: vec!["project32".into(), "windUniforms".into()],
                    builtin_prelude: None,
                },
                ShaderBinding {
                    pattern: "blit.*.glsl".into(),
                    modules: vec!["blitUniforms".into()],
                    builtin_prelude: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn binding_resolves_each_shader_to_its_own_modules() {
        let base = Path::new("/proj");
        // draw -> project32 (builtin) + windUniforms (a file), no blit module.
        let draw = resolve_bindings(Path::new("/proj/src/shaders/draw.vert.glsl"), &cf(), base);
        assert!(draw.use_builtin_prelude);
        assert_eq!(
            draw.modules,
            vec![PathBuf::from("/proj/src/shaders/windUniforms.glsl")]
        );

        // blit -> only blitUniforms, and NOT the deck prelude (it uses no project).
        let blit = resolve_bindings(Path::new("/proj/src/shaders/blit.frag.glsl"), &cf(), base);
        assert!(!blit.use_builtin_prelude);
        assert_eq!(
            blit.modules,
            vec![PathBuf::from("/proj/src/shaders/blitUniforms.glsl")]
        );
    }

    #[test]
    fn unmatched_shader_gets_only_the_prelude() {
        let cfg = resolve_bindings(
            Path::new("/proj/src/shaders/windUniforms.glsl"),
            &cf(),
            Path::new("/proj"),
        );
        assert!(cfg.modules.is_empty());
    }
}
