//! The core trick: luma.gl/deck.gl shaders aren't standalone translation units.
//! They reference UBO instances (`wind.*`, `blit.*`) declared in separate module
//! fragments and deck builtins (`project_position_to_clipspace`) injected at link
//! time. This module splices those in to form a complete `#version 300 es` unit a
//! validator will accept, while recording a per-line map back to the originals so
//! diagnostics land where the author can act on them.

use crate::config::Config;
use naga::ShaderStage;
use std::path::{Path, PathBuf};

/// deck.gl `project32` stubs. Bodies are trivial — only the signatures matter
/// for type/semantic checking of the consumer shader.
pub const BUILTIN_PRELUDE: &str = r#"// glslint built-in prelude: deck.gl project32
vec4 project_position_to_clipspace(vec3 position, vec3 position64Low, vec3 offset) {
  return vec4(position + position64Low + offset, 1.0);
}
vec2 project_pixel_size_to_clipspace(vec2 pixels) { return pixels; }
vec3 project_position(vec3 position) { return position; }
vec4 project_common_position_to_clipspace(vec4 position) { return position; }
"#;

/// naga's GLSL frontend only accepts desktop *core* (versions 440/450/460), not
/// GLSL ES. WebGL2 shaders are `#version 300 es`, whose feature set the shaders
/// here use is a subset of 4.60 core — so we emit a core version directive and
/// normalize the ES-only spellings below.
const CORE_VERSION: &str = "#version 460 core";

/// Where an assembled line came from. `line` is 1-based into `path`.
#[derive(Debug, Clone)]
pub struct Loc {
    pub path: PathBuf,
    pub line: u32,
}

pub struct Assembled {
    pub source: String,
    pub stage: ShaderStage,
    /// One entry per assembled line: assembled line `i+1` -> `map[i]`. `None` for
    /// synthetic/injected-prelude lines we own (errors there are dropped).
    pub map: Vec<Option<Loc>>,
    pub target: PathBuf,
    /// Set when the file was wrapped because it's a module fragment, not a stage.
    /// Reserved for an info-level diagnostic once the LSP grows one.
    #[allow(dead_code)]
    pub note: Option<&'static str>,
}

struct Builder {
    lines: Vec<String>,
    map: Vec<Option<Loc>>,
}

impl Builder {
    fn new() -> Self {
        Builder { lines: Vec::new(), map: Vec::new() }
    }
    fn push(&mut self, line: String, loc: Option<Loc>) {
        self.lines.push(line);
        self.map.push(loc);
    }
    /// Append a block from `path`, mapping each line back to it.
    fn push_block(&mut self, content: &str, path: &Path) {
        for (i, l) in content.lines().enumerate() {
            self.push(l.to_string(), Some(Loc { path: path.to_path_buf(), line: i as u32 + 1 }));
        }
    }
    /// Append lines we synthesized; errors here map nowhere.
    fn push_synthetic(&mut self, content: &str) {
        for l in content.lines() {
            self.push(l.to_string(), None);
        }
    }
    fn finish(mut self, stage: ShaderStage, target: &Path, note: Option<&'static str>) -> Assembled {
        normalize_core(&mut self.lines);
        let mut source = self.lines.join("\n");
        source.push('\n');
        Assembled { source, stage, map: self.map, target: target.to_path_buf(), note }
    }
}

/// Rewrite ES-3.00 spellings naga's core frontend can't ingest, preserving line
/// count so the diagnostic map stays valid.
fn normalize_core(lines: &mut [String]) {
    let mut binding = 0u32;
    for line in lines.iter_mut() {
        let trimmed = line.trim();
        // ES `precision ...;` statements are no-ops in core and unsupported by
        // naga — blank the line (keeping it for the line map).
        if trimmed.starts_with("precision ") && trimmed.ends_with(';') {
            line.clear();
            continue;
        }
        // Inline precision qualifiers are equally redundant in core.
        for q in ["highp ", "mediump ", "lowp "] {
            if line.contains(q) {
                *line = line.replace(q, "");
            }
        }
        // naga(core) demands an explicit binding on every interface block.
        for layout in ["layout(std140)", "layout(std430)"] {
            if line.contains(layout) && !line.contains("binding") {
                let bound = layout.replace(')', &format!(", binding={binding})"));
                *line = line.replace(layout, &bound);
                binding += 1;
            }
        }
    }
}

/// Infer the shader stage from the filename. `None` => not a stage shader (a
/// module fragment like `windUniforms.glsl`), which we wrap for syntax-checking.
pub fn detect_stage(path: &Path) -> Option<ShaderStage> {
    let name = path.file_name()?.to_str()?;
    let n = name.to_ascii_lowercase();
    if n.contains(".vert.") || n.ends_with(".vert") || n.ends_with(".vs") {
        Some(ShaderStage::Vertex)
    } else if n.contains(".frag.") || n.ends_with(".frag") || n.ends_with(".fs") {
        Some(ShaderStage::Fragment)
    } else if n.contains(".comp.") || n.ends_with(".comp") {
        Some(ShaderStage::Compute)
    } else {
        None
    }
}

pub fn assemble(target: &Path, source: &str, config: &Config) -> Assembled {
    match detect_stage(target) {
        Some(stage) => assemble_stage(target, source, config, stage),
        None => wrap_fragment(target, source),
    }
}

fn assemble_stage(target: &Path, source: &str, config: &Config, stage: ShaderStage) -> Assembled {
    let mut b = Builder::new();
    let lines: Vec<&str> = source.lines().collect();

    // Emit a core version directive first (naga rejects `#version 300 es`); the
    // original directive is dropped from the body below.
    let vidx = lines.iter().position(|l| l.trim_start().starts_with("#version"));
    b.push_synthetic(CORE_VERSION);

    if config.use_builtin_prelude {
        b.push_synthetic(BUILTIN_PRELUDE);
    }
    for p in &config.preludes {
        if let Ok(c) = std::fs::read_to_string(p) {
            b.push_block(&c, p);
        }
    }
    // Inject every configured module fragment except the file under check.
    for m in &config.modules {
        if same_file(m, target) {
            continue;
        }
        if let Ok(c) = std::fs::read_to_string(m) {
            b.push_block(&c, m);
        }
    }

    // The rest of the original, every line except the hoisted `#version`.
    for (i, l) in lines.iter().enumerate() {
        if Some(i) == vidx {
            continue;
        }
        b.push(l.to_string(), Some(Loc { path: target.to_path_buf(), line: i as u32 + 1 }));
    }

    b.finish(stage, target, None)
}

/// A module fragment (a bare UBO block, no stage / no `main`) can't be a shader
/// on its own. Wrap it in a minimal fragment shell so its declarations still get
/// a real syntax/type pass.
fn wrap_fragment(target: &Path, source: &str) -> Assembled {
    let mut b = Builder::new();
    b.push_synthetic(CORE_VERSION);
    for (i, l) in source.lines().enumerate() {
        b.push(l.to_string(), Some(Loc { path: target.to_path_buf(), line: i as u32 + 1 }));
    }
    b.push_synthetic("void main() {}");
    b.finish(ShaderStage::Fragment, target, Some("module fragment (syntax-only)"))
}

/// True if two paths point at the same file. Canonicalize when possible; fall
/// back to a literal compare for not-yet-existing paths.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
