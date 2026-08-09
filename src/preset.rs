//! Presets: shader-ecosystem knowledge as *data*, not code.
//!
//! The linter core is ecosystem-agnostic — it discovers a project's shared library
//! and injected defines by structure (see [`crate::discover`]). A preset is the
//! optional fast/precise layer on top: it declares how to recognize an ecosystem,
//! the `#pragma` transform the core can't infer, and (for speed) the exact library
//! files and injected macros so the core can skip discovery.
//!
//! Crucially, a bundled preset (`presets/maplibre.toml`, compiled in) and a
//! project's own `glslint.toml` at its root use the **identical schema**
//! ([`schema/glslint.schema.json`]). So a project defines a preset exactly the way
//! glslint ships one, and all maplibre-specific knowledge lives in
//! `presets/maplibre.toml` — none in Rust.

use crate::dialect::{Dialect, Rule};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A parsed preset: how to recognize the ecosystem, plus the [`Dialect`] to apply.
#[derive(Debug, Clone)]
pub struct Preset {
    pub detect: Detect,
    pub dialect: Dialect,
}

/// When a preset applies to a shader.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// Any of these as a substring of the shader source activates the preset.
    #[serde(default)]
    pub source_contains: Vec<String>,
    /// Any of these filenames existing next to the shader activates the preset
    /// (so pragma-free shaders in a known project are still recognized).
    #[serde(default)]
    pub sibling_files: Vec<String>,
}

impl Detect {
    fn matches(&self, source: &str, dir: &Path) -> bool {
        self.source_contains.iter().any(|s| source.contains(s))
            || self.sibling_files.iter().any(|f| dir.join(f).is_file())
    }
}

// --- On-disk schema (identical for bundled presets and a project `glslint.toml`) ---

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFile {
    name: String,
    #[serde(default)]
    detect: Detect,
    /// Shared-library files the build concatenates ahead of every shader.
    #[serde(default)]
    library: LibrarySpec,
    /// Names of macros the project injects from JS at build time; glslint supplies
    /// an `#ifndef`-guarded default so they resolve.
    #[serde(default)]
    defines: Vec<String>,
    /// Opt in to discovering injected `#define NAME ${…}` macros by scanning the
    /// project's JS/TS, on top of `defines`.
    #[serde(default)]
    discover_defines: bool,
    /// Static preludes (implicit globals).
    prelude: Option<String>,
    prelude_vertex: Option<String>,
    prelude_fragment: Option<String>,
    /// A wrapper `main` appended when the source has none (ShaderToy's `mainImage`).
    epilogue: Option<String>,
    /// Whether the deck.gl builtin prelude still applies (default false).
    #[serde(default)]
    deck: bool,
    /// Directive-expansion rules (the `#pragma` DSL).
    #[serde(default, rename = "expand")]
    expand: Vec<ExpandRuleFile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibrarySpec {
    #[serde(default)]
    vertex: Vec<String>,
    #[serde(default)]
    fragment: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandRuleFile {
    /// Pragma namespace(s): one string or a list (maplibre uses both
    /// `maplibre` and `mapbox`).
    pragma: OneOrMany,
    verb: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    /// Any-stage template; stage fields override it.
    emit: Option<String>,
    vertex: Option<String>,
    fragment: Option<String>,
    compute: Option<String>,
}

/// A field that accepts either a single string or a list of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

/// Parse one preset's TOML into a runtime [`Preset`].
pub fn parse(text: &str) -> Result<Preset, toml::de::Error> {
    let pf: PresetFile = toml::from_str(text)?;
    Ok(build(pf))
}

fn build(pf: PresetFile) -> Preset {
    let mut rules = Vec::new();
    for e in pf.expand {
        for ns in e.pragma.clone().into_vec() {
            rules.push(Rule {
                ns,
                verb: e.verb.clone(),
                args: e.args.clone(),
                vertex: e.vertex.clone(),
                fragment: e.fragment.clone(),
                compute: e.compute.clone(),
                any: e.emit.clone(),
            });
        }
    }
    let dialect = Dialect {
        name: pf.name,
        prelude_vertex: pf.prelude_vertex,
        prelude_fragment: pf.prelude_fragment,
        prelude_any: pf.prelude,
        prelude_extra: None,
        epilogue: pf.epilogue,
        prelude_files_vertex: pf.library.vertex,
        prelude_files_fragment: pf.library.fragment,
        define_names: pf.defines,
        discover_defines: pf.discover_defines,
        rules,
        deck: pf.deck,
    };
    Preset {
        detect: pf.detect,
        dialect,
    }
}

/// The presets compiled into the binary. Parsed once.
pub fn bundled() -> &'static [Preset] {
    static BUNDLED: OnceLock<Vec<Preset>> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        [
            include_str!("../presets/maplibre.toml"),
            include_str!("../presets/shadertoy.toml"),
            include_str!("../presets/deck.toml"),
        ]
        .iter()
        .filter_map(|t| parse(t).ok())
        .collect()
    })
}

/// The `deck` preset's fallback stub prelude — deck's `project*` builtins as
/// type-correct stubs, used when the real signatures can't be resolved from
/// node_modules. Empty if the bundled deck preset is somehow missing.
pub fn deck_prelude() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        bundled()
            .iter()
            .find(|p| p.dialect.name == "deck")
            .and_then(|p| p.dialect.prelude_any.clone())
            .unwrap_or_default()
    })
}

/// Presets a project defines in a `glslint.toml` at (or above) `dir`. A single
/// preset per file for now; missing/unparseable file yields none.
pub fn project(dir: &Path) -> Vec<Preset> {
    let Some(path) = find_up(dir, "glslint.toml") else {
        return Vec::new();
    };
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| parse(&t).ok())
    {
        Some(p) => vec![p],
        None => Vec::new(),
    }
}

/// Resolve a preset's dialect by explicit `name` (project presets shadow bundled).
pub fn by_name(name: &str, dir: &Path) -> Option<Dialect> {
    if name == "none" || name == "raw" {
        return Some(Dialect {
            name: "none".into(),
            deck: false,
            ..Default::default()
        });
    }
    project(dir)
        .into_iter()
        .find(|p| p.dialect.name == name)
        .map(|p| p.dialect)
        .or_else(|| {
            bundled()
                .iter()
                .find(|p| p.dialect.name == name)
                .map(|p| p.dialect.clone())
        })
}

/// Detect the dialect for a shader from `source` and its directory. Project
/// presets are tried before bundled ones, so a project can override a shipped
/// preset; the first match wins.
pub fn detect(source: &str, dir: &Path) -> Option<Dialect> {
    for p in project(dir) {
        if p.detect.matches(source, dir) {
            return Some(p.dialect);
        }
    }
    bundled()
        .iter()
        .find(|p| p.detect.matches(source, dir))
        .map(|p| p.dialect.clone())
}

/// Walk up from `start` to a `.git` root looking for `name`.
fn find_up(start: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::assemble::Stage;

    #[test]
    fn bundled_presets_parse() {
        let names: Vec<&str> = bundled().iter().map(|p| p.dialect.name.as_str()).collect();
        assert!(names.contains(&"maplibre"), "got {names:?}");
        assert!(names.contains(&"shadertoy"), "got {names:?}");
        assert!(names.contains(&"deck"), "got {names:?}");
    }

    #[test]
    fn deck_preset_provides_stub_prelude_and_detection() {
        // The stub prelude (once hardcoded in assemble.rs) is now data in the deck
        // preset, and a `deck = true` dialect that opts into node_modules resolution.
        let bare = Path::new("/nonexistent-glslint");
        let d = by_name("deck", bare).unwrap();
        assert!(d.deck);
        assert!(deck_prelude().contains("project_position_to_clipspace"));
        // A shader calling a deck builtin is detected as deck.
        assert_eq!(
            detect(
                "void main() { gl_Position = project_position_to_clipspace(a, b, c); }",
                bare
            )
            .map(|d| d.name),
            Some("deck".into())
        );
    }

    #[test]
    fn maplibre_preset_carries_its_knowledge() {
        let d = by_name("maplibre", Path::new("/nonexistent")).unwrap();
        // The pragma DSL, both namespaces.
        assert!(
            d.rules
                .iter()
                .any(|r| r.ns == "maplibre" && r.verb.as_deref() == Some("define"))
        );
        assert!(d.rules.iter().any(|r| r.ns == "mapbox"));
        // The explicit shared library and injected define.
        assert!(
            d.prelude_files_vertex
                .iter()
                .any(|f| f.contains("_projection_mercator"))
        );
        assert!(
            d.define_names
                .iter()
                .any(|n| n == "NUM_ILLUMINATION_SOURCES")
        );
        // And the expansion actually works.
        let v = d
            .expand_line("#pragma maplibre: define lowp float opacity", Stage::Vertex)
            .unwrap();
        assert!(v.iter().any(|l| l == "out lowp float opacity;"));
    }

    #[test]
    fn detect_uses_source_and_sibling_signals() {
        let bare = Path::new("/nonexistent-glslint");
        assert_eq!(
            detect("#pragma maplibre: define lowp float x", bare).map(|d| d.name),
            Some("maplibre".into())
        );
        assert_eq!(
            detect("void mainImage(out vec4 c, in vec2 p) {}", bare).map(|d| d.name),
            Some("shadertoy".into())
        );
        assert_eq!(detect("void main(){}", bare).map(|d| d.name), None);
    }

    #[test]
    fn one_or_many_accepts_both_forms() {
        let one = parse("name='a'\n[[expand]]\npragma='x'\nemit='y'\n").unwrap();
        let many = parse("name='b'\n[[expand]]\npragma=['x','z']\nemit='y'\n").unwrap();
        assert_eq!(one.dialect.rules.len(), 1);
        assert_eq!(many.dialect.rules.len(), 2);
    }
}
