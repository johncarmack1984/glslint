//! Per-file configuration: which preludes and luma/deck module fragments to
//! splice in before validating. Resolved from a `glsl-lsp.toml` found by walking
//! up from the target file, or sensible zero-config defaults.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigFile {
    /// Extra GLSL files prepended verbatim (after `#version`, before modules).
    #[serde(default)]
    preludes: Vec<String>,
    /// luma.gl shader-module fragments (e.g. the std140 UBO blocks) injected
    /// into every stage shader so `wind.*` / `blit.*` resolve.
    #[serde(default)]
    modules: Vec<String>,
    /// Prepend the baked-in deck.gl `project32` + ES-precision prelude.
    builtin_prelude: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub preludes: Vec<PathBuf>,
    pub modules: Vec<PathBuf>,
    pub use_builtin_prelude: bool,
}

impl Config {
    /// Resolve config for `file`. Infallible: any IO/parse error degrades to the
    /// zero-config default rather than failing the check.
    pub fn resolve_for(file: &Path) -> Config {
        let dir = file.parent().unwrap_or(Path::new("."));

        if let Some(toml_path) = find_up(dir, "glsl-lsp.toml") {
            if let Ok(text) = std::fs::read_to_string(&toml_path) {
                if let Ok(cf) = toml::from_str::<ConfigFile>(&text) {
                    let base = toml_path.parent().unwrap_or(Path::new("."));
                    return Config {
                        preludes: cf.preludes.iter().map(|p| base.join(p)).collect(),
                        modules: cf.modules.iter().map(|m| base.join(m)).collect(),
                        use_builtin_prelude: cf.builtin_prelude.unwrap_or(true),
                    };
                }
            }
        }

        // Zero-config: builtin deck/luma prelude + auto-discovered sibling UBO
        // fragments (`*Uniforms.glsl`) next to the target.
        Config {
            preludes: Vec::new(),
            modules: discover_sibling_modules(dir),
            use_builtin_prelude: true,
        }
    }
}

fn discover_sibling_modules(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("Uniforms.glsl"))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
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
