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
        Builder {
            lines: Vec::new(),
            map: Vec::new(),
        }
    }
    fn push(&mut self, line: String, loc: Option<Loc>) {
        self.lines.push(line);
        self.map.push(loc);
    }
    /// Append a block from `path`, mapping each line back to it.
    fn push_block(&mut self, content: &str, path: &Path) {
        for (i, l) in content.lines().enumerate() {
            self.push(
                l.to_string(),
                Some(Loc {
                    path: path.to_path_buf(),
                    line: line_no(i),
                }),
            );
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
        Assembled {
            source,
            stage,
            map: self.map,
            target: target.to_path_buf(),
            note,
        }
    }
}

/// Infer the shader stage from the filename. `None` => not a stage shader (a
/// module fragment like `windUniforms.glsl`), which we wrap for syntax-checking.
pub fn detect_stage(path: &Path) -> Option<Stage> {
    let name = path.file_name()?.to_str()?;
    let n = name.to_ascii_lowercase();
    if n.contains(".vert.")
        || n.contains(".vertex.")
        || n.ends_with(".vert")
        || n.ends_with(".vertex")
        || n.ends_with(".vs")
    {
        Some(Stage::Vertex)
    } else if n.contains(".frag.")
        || n.contains(".fragment.")
        || n.ends_with(".frag")
        || n.ends_with(".fragment")
        || n.ends_with(".fs")
    {
        Some(Stage::Fragment)
    } else if n.contains(".comp.")
        || n.contains(".compute.")
        || n.ends_with(".comp")
        || n.ends_with(".compute")
    {
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

/// Assemble a shader lifted from a JS/TS tagged template. The stage can't come
/// from the filename here (the host is a `.ts`/`.js` file), so it's passed in —
/// inferred from the shader's own text/binding by `embed`. `has_entry` is whether
/// the template has its own `main`: with one, it's a full stage shader; without,
/// it's a chunk wrapped in a synthetic `main` for syntax-only checking. Either way
/// the wrap/validate happens under `stage`, so a vertex-only builtin (e.g.
/// `gl_VertexID`) in a `gl_Position`-free shader isn't rejected under the fragment
/// stage. `target` is the host file, so every mapped line points there; `embed`
/// then offsets those lines into the template's span.
pub fn assemble_embedded(
    target: &Path,
    source: &str,
    config: &Config,
    stage: Stage,
    has_entry: bool,
) -> Assembled {
    if has_entry {
        assemble_stage(target, source, config, stage)
    } else {
        wrap_as(target, source, stage)
    }
}

fn assemble_stage(target: &Path, source: &str, config: &Config, stage: Stage) -> Assembled {
    let mut b = Builder::new();
    let lines: Vec<&str> = source.lines().collect();

    // Resolve the shader dialect (maplibre pragmas, shadertoy uniforms, project
    // rules) for this shader — detection keys on the source and its directory.
    // `None` => no dialect handling, the deck/luma path.
    let dir = target.parent().unwrap_or(Path::new("."));
    let dialect = crate::dialect::resolve(&config.dialect, source, dir);

    // A `#version` directive must precede all code, so hoist the target's own to
    // the top (it's dropped from the body below) and map it back to its real line
    // so a version error still points home. Default it when absent. Default
    // precision follows, before any prelude — see DEFAULT_PRECISION.
    let vidx = lines
        .iter()
        .position(|l| l.trim_start().starts_with("#version"));
    match vidx {
        Some(i) => b.push(
            lines[i].to_string(),
            Some(Loc {
                path: target.to_path_buf(),
                line: line_no(i),
            }),
        ),
        None => b.push_synthetic(DEFAULT_VERSION),
    }
    b.push_synthetic(DEFAULT_PRECISION);

    // A dialect's prelude declares its implicit globals (shadertoy's `iTime` &c.).
    if let Some(d) = &dialect
        && let Some(p) = d.prelude(stage)
    {
        b.push_synthetic(p);
    }
    if let Some(d) = &dialect
        && let Some(p) = &d.prelude_extra
    {
        b.push_synthetic(p);
    }

    // A dialect's shared shader library: sibling files the ecosystem's build
    // concatenates ahead of every shader (maplibre's `_prelude`/`_projection_*`).
    // Injected verbatim and mapped to their own path, so `projectTile` &c. resolve
    // and a diagnostic inside one lands on that file, not the shader under check.
    if let Some(d) = &dialect {
        for name in d.prelude_files(stage) {
            let path = dir.join(name);
            if same_file(&path, target) {
                continue;
            }
            if let Ok(c) = std::fs::read_to_string(&path) {
                b.push_block(&c, &path);
            }
        }
    }

    // The deck builtin prelude applies unless a non-deck dialect took over.
    let deck_applies = dialect.as_ref().is_none_or(|d| d.deck);
    if config.use_builtin_prelude && deck_applies {
        // Prefer deck's real project functions from node_modules (extracted as
        // empty-body stubs); fall back to the baked-in 4-function stub.
        let fns = crate::deck::project_fns(target.parent().unwrap_or(Path::new(".")));
        if fns.is_empty() {
            b.push_synthetic(BUILTIN_PRELUDE);
        } else {
            b.push_synthetic(&crate::deck::stubs(&fns));
        }
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

    // The rest of the original, every line except the hoisted `#version`. A dialect
    // may expand a directive line into several declarations; each keeps the loc of
    // the pragma it came from, so a diagnostic inside the expansion points at the
    // author's `#pragma`.
    for (i, l) in lines.iter().enumerate() {
        if Some(i) == vidx {
            continue;
        }
        let loc = Loc {
            path: target.to_path_buf(),
            line: line_no(i),
        };
        match dialect.as_ref().and_then(|d| d.expand_line(l, stage)) {
            Some(expanded) => {
                for e in expanded {
                    b.push(e, Some(loc.clone()));
                }
            }
            None => b.push(l.to_string(), Some(loc)),
        }
    }

    // A dialect epilogue (shadertoy's `main` driving `mainImage`) is appended only
    // when the source has no entry point of its own.
    if let Some(d) = &dialect
        && let Some(ep) = &d.epilogue
        && !crate::dialect::has_main(source)
    {
        b.push_synthetic(ep);
    }

    b.finish(stage, target, None)
}

/// A module fragment (a bare UBO block, no stage / no `main`) can't be a shader
/// on its own. Wrap it in a minimal fragment shell so its declarations still get
/// a real syntax/type pass.
fn wrap_fragment(target: &Path, source: &str) -> Assembled {
    wrap_as(target, source, Stage::Fragment)
}

/// Wrap a fragment (no `main` of its own) in a minimal shell under `stage`, so its
/// declarations get a syntax/type pass. The stage matters: a helper that uses a
/// vertex-only builtin must be wrapped as a vertex shader, not a fragment one.
fn wrap_as(target: &Path, source: &str, stage: Stage) -> Assembled {
    let mut b = Builder::new();
    b.push_synthetic(DEFAULT_VERSION);
    b.push_synthetic(DEFAULT_PRECISION);
    for (i, l) in source.lines().enumerate() {
        b.push(
            l.to_string(),
            Some(Loc {
                path: target.to_path_buf(),
                line: line_no(i),
            }),
        );
    }
    b.push_synthetic("void main() {}");
    b.finish(stage, target, Some("module fragment (syntax-only)"))
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
#[allow(clippy::unwrap_used)] // test code: unwrap IS the assertion
mod tests {
    use super::*;

    fn no_config() -> Config {
        Config {
            preludes: vec![],
            modules: vec![],
            use_builtin_prelude: false,
            dialect: crate::dialect::Preference::default(),
        }
    }

    #[test]
    fn detect_stage_reads_the_filename() {
        assert_eq!(
            detect_stage(Path::new("draw.vert.glsl")),
            Some(Stage::Vertex)
        );
        assert_eq!(
            detect_stage(Path::new("draw.frag.glsl")),
            Some(Stage::Fragment)
        );
        assert_eq!(
            detect_stage(Path::new("sim.comp.glsl")),
            Some(Stage::Compute)
        );
        // The spelled-out convention (maplibre-gl-js and friends).
        assert_eq!(
            detect_stage(Path::new("line.vertex.glsl")),
            Some(Stage::Vertex)
        );
        assert_eq!(
            detect_stage(Path::new("line.fragment.glsl")),
            Some(Stage::Fragment)
        );
        // A bare module fragment is not a stage shader.
        assert_eq!(detect_stage(Path::new("windUniforms.glsl")), None);
    }

    #[test]
    fn spelled_out_vertex_stage_is_not_wrapped_as_fragment() {
        // A vertex-stage integer input needs no `flat`; the None fallback wraps as
        // a fragment shader, where the same declaration is an error. Regression
        // for maplibre-style `*.vertex.glsl` naming.
        let src = "#version 300 es\nlayout(location = 0) in ivec2 a_pos_normal;\nvoid main() { gl_Position = vec4(vec2(a_pos_normal >> 1), 0.0, 1.0); }\n";
        let a = assemble(Path::new("line.vertex.glsl"), src, &no_config());
        assert_eq!(a.stage, Stage::Vertex);
    }

    #[test]
    fn line_map_has_exactly_one_entry_per_assembled_line() {
        // The whole diagnostic translation indexes `map[asm_line - 1]`, so the map
        // must stay 1:1 with the assembled source's lines.
        let src =
            "#version 300 es\nprecision highp float;\nout vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
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
    fn maplibre_pragma_is_expanded_and_stays_one_to_one_mapped() {
        // Auto-detected maplibre define/initialize expand into real declarations,
        // and every expanded line still has exactly one map entry so the diagnostic
        // translation can't drift.
        let src = "#pragma maplibre: define lowp float opacity\n\
                   void main() {\n\
                   #pragma maplibre: initialize lowp float opacity\n\
                   gl_Position = vec4(opacity);\n\
                   }\n";
        let a = assemble(Path::new("line.vertex.glsl"), src, &no_config());
        assert_eq!(a.stage, Stage::Vertex);
        assert_eq!(a.source.lines().count(), a.map.len());
        // The define produced the attribute/varying; the pragma line itself is gone.
        assert!(a.source.contains("in lowp float a_opacity;"));
        assert!(a.source.contains("out lowp float opacity;"));
        assert!(!a.source.contains("#pragma maplibre"));
        // An expanded declaration maps back to the pragma's original line (line 1).
        let idx = a
            .source
            .lines()
            .position(|l| l.contains("out lowp float opacity;"))
            .unwrap();
        assert_eq!(a.map[idx].as_ref().unwrap().line, 1);
    }

    #[test]
    fn no_dialect_leaves_a_plain_shader_untouched() {
        // A deck/luma shader without dialect signatures gets no expansion.
        let src = "out vec4 c;\nvoid main(){ c = vec4(1.0); }\n";
        let a = assemble(Path::new("draw.frag.glsl"), src, &no_config());
        assert!(a.source.contains("out vec4 c;"));
        assert_eq!(a.source.lines().count(), a.map.len());
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

    #[test]
    fn maplibre_injects_its_sibling_shared_library_mapped_to_its_own_file() {
        // A pragma-free maplibre shader next to the `_prelude.*.glsl` library gets
        // that library spliced in (so `projectTile` resolves), each injected line
        // mapped back to the library file — not the shader — and the map stays 1:1.
        let dir = std::env::temp_dir().join(format!(
            "glslint-asm-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = "vec4 projectTile(vec2 p) { return vec4(p, 0.0, 1.0); }\n";
        std::fs::write(dir.join("_prelude.vertex.glsl"), lib).unwrap();
        let shader = dir.join("background.vertex.glsl");
        let src = "void main() { gl_Position = projectTile(vec2(0.0)); }\n";

        let a = assemble(&shader, src, &no_config());
        assert!(
            a.source.contains("vec4 projectTile(vec2 p)"),
            "shared library was injected: {}",
            a.source
        );
        assert_eq!(a.source.lines().count(), a.map.len());
        let idx = a
            .source
            .lines()
            .position(|l| l.contains("projectTile(vec2 p)"))
            .unwrap();
        assert!(
            a.map[idx]
                .as_ref()
                .unwrap()
                .path
                .ends_with("_prelude.vertex.glsl")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
}
