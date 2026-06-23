//! The core trick: luma.gl/deck.gl shaders aren't standalone translation units.
//! They reference UBO instances (`wind.*`, `blit.*`) declared in separate module
//! fragments and deck builtins (`project_position_to_clipspace`) injected at link
//! time. This module splices those in to form a complete `#version 300 es` unit a
//! validator will accept, while recording a per-line map back to the originals so
//! diagnostics land where the author can act on them.

use crate::config::Config;
use std::path::{Path, PathBuf};

/// Shader stage, inferred from the filename. Maps to glslangValidator's `-S` arg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

impl Stage {
    /// The `-S <stage>` argument glslangValidator expects.
    pub fn glslang_stage(self) -> &'static str {
        match self {
            Stage::Vertex => "vert",
            Stage::Fragment => "frag",
            Stage::Compute => "comp",
        }
    }
}

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

/// Used when the target has no `#version` of its own. WebGL2/luma shaders are
/// GLSL ES 3.00; glslangValidator validates that profile natively (combined
/// samplers, combined-sampler function params, and all) with no source rewrites.
const DEFAULT_VERSION: &str = "#version 300 es";

/// Injected right after the (hoisted) `#version`, before any prelude. GLSL ES
/// fragment shaders have no default `float` precision, and the deck prelude stubs
/// reference `float`/`vec*` ahead of the target's own `precision` statement — so
/// we set defaults up front. Re-declaring them later (as the shaders do) is legal.
const DEFAULT_PRECISION: &str = "precision highp float;\nprecision highp int;";

/// Where an assembled line came from. `line` is 1-based into `path`.
#[derive(Debug, Clone)]
pub struct Loc {
    pub path: PathBuf,
    pub line: u32,
}

pub struct Assembled {
    pub source: String,
    pub stage: Stage,
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
            self.push(l.to_string(), Some(Loc { path: path.to_path_buf(), line: line_no(i) }));
        }
    }
    /// Append lines we synthesized; errors here map nowhere.
    fn push_synthetic(&mut self, content: &str) {
        for l in content.lines() {
            self.push(l.to_string(), None);
        }
    }
    fn finish(self, stage: Stage, target: &Path, note: Option<&'static str>) -> Assembled {
        let mut source = self.lines.join("\n");
        source.push('\n');
        Assembled { source, stage, map: self.map, target: target.to_path_buf(), note }
    }
}

/// Infer the shader stage from the filename. `None` => not a stage shader (a
/// module fragment like `windUniforms.glsl`), which we wrap for syntax-checking.
pub fn detect_stage(path: &Path) -> Option<Stage> {
    let name = path.file_name()?.to_str()?;
    let n = name.to_ascii_lowercase();
    if n.contains(".vert.") || n.ends_with(".vert") || n.ends_with(".vs") {
        Some(Stage::Vertex)
    } else if n.contains(".frag.") || n.ends_with(".frag") || n.ends_with(".fs") {
        Some(Stage::Fragment)
    } else if n.contains(".comp.") || n.ends_with(".comp") {
        Some(Stage::Compute)
    } else {
        None
    }
}

/// 1-based line number for a 0-based index. Saturating, so a pathological
/// (>4-billion-line) file can't wrap a line number into a wrong-but-plausible one.
pub(crate) fn line_no(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1)
}

pub fn assemble(target: &Path, source: &str, config: &Config) -> Assembled {
    match detect_stage(target) {
        Some(stage) => assemble_stage(target, source, config, stage),
        None => wrap_fragment(target, source),
    }
}

fn assemble_stage(target: &Path, source: &str, config: &Config, stage: Stage) -> Assembled {
    let mut b = Builder::new();
    let lines: Vec<&str> = source.lines().collect();

    // A `#version` directive must precede all code, so hoist the target's own to
    // the top (it's dropped from the body below) and map it back to its real line
    // so a version error still points home. Default it when absent. Default
    // precision follows, before any prelude — see DEFAULT_PRECISION.
    let vidx = lines.iter().position(|l| l.trim_start().starts_with("#version"));
    match vidx {
        Some(i) => b.push(
            lines[i].to_string(),
            Some(Loc { path: target.to_path_buf(), line: line_no(i) }),
        ),
        None => b.push_synthetic(DEFAULT_VERSION),
    }
    b.push_synthetic(DEFAULT_PRECISION);

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
        b.push(l.to_string(), Some(Loc { path: target.to_path_buf(), line: line_no(i) }));
    }

    b.finish(stage, target, None)
}

/// A module fragment (a bare UBO block, no stage / no `main`) can't be a shader
/// on its own. Wrap it in a minimal fragment shell so its declarations still get
/// a real syntax/type pass.
fn wrap_fragment(target: &Path, source: &str) -> Assembled {
    let mut b = Builder::new();
    b.push_synthetic(DEFAULT_VERSION);
    b.push_synthetic(DEFAULT_PRECISION);
    for (i, l) in source.lines().enumerate() {
        b.push(l.to_string(), Some(Loc { path: target.to_path_buf(), line: line_no(i) }));
    }
    b.push_synthetic("void main() {}");
    b.finish(Stage::Fragment, target, Some("module fragment (syntax-only)"))
}

/// True if two paths point at the same file. Canonicalize when possible; fall
/// back to a literal compare for not-yet-existing paths.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_config() -> Config {
        Config { preludes: vec![], modules: vec![], use_builtin_prelude: false }
    }

    #[test]
    fn detect_stage_reads_the_filename() {
        assert_eq!(detect_stage(Path::new("draw.vert.glsl")), Some(Stage::Vertex));
        assert_eq!(detect_stage(Path::new("draw.frag.glsl")), Some(Stage::Fragment));
        assert_eq!(detect_stage(Path::new("sim.comp.glsl")), Some(Stage::Compute));
        // A bare module fragment is not a stage shader.
        assert_eq!(detect_stage(Path::new("windUniforms.glsl")), None);
    }

    #[test]
    fn line_map_has_exactly_one_entry_per_assembled_line() {
        // The whole diagnostic translation indexes `map[asm_line - 1]`, so the map
        // must stay 1:1 with the assembled source's lines.
        let src = "#version 300 es\nprecision highp float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
        let a = assemble(Path::new("draw.frag.glsl"), src, &no_config());
        assert_eq!(a.source.lines().count(), a.map.len());
    }

    #[test]
    fn version_is_hoisted_and_mapped_to_its_original_line() {
        let src = "#version 300 es\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
        let a = assemble(Path::new("draw.frag.glsl"), src, &no_config());
        assert!(a.source.starts_with("#version 300 es"));
        // Assembled line 1 (the hoisted directive) maps back to original line 1.
        assert_eq!(a.map[0].as_ref().unwrap().line, 1);
    }

    #[test]
    fn default_precision_is_injected() {
        let src = "#version 300 es\nout vec4 c;\nvoid main(){}\n";
        let a = assemble(Path::new("draw.frag.glsl"), src, &no_config());
        assert!(a.source.contains("precision highp float;"));
    }

    #[test]
    fn bare_module_fragment_is_wrapped_for_syntax_checking() {
        // No stage in the name → wrapped with a synthetic main, still 1:1 mapped.
        let src = "layout(std140) uniform U { float a; } u;\n";
        let a = assemble(Path::new("windUniforms.glsl"), src, &no_config());
        assert_eq!(a.stage, Stage::Fragment);
        assert!(a.source.contains("void main()"));
        assert_eq!(a.source.lines().count(), a.map.len());
    }
}
