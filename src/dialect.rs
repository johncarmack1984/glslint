//! Shader "dialects": the third way a `.glsl` file fails to be a standalone
//! translation unit.
//!
//! `assemble` already handles the first two — implicit globals injected at link
//! time (deck builtins, via the prelude) and declarations that live in sibling
//! module fragments (luma/deck UBOs, via `[[module]]` bindings). The third is an
//! **in-file directive DSL**: pragmas an ecosystem's own build step expands into
//! real declarations before any compiler sees the file. maplibre-gl-js is the
//! canonical case — `#pragma maplibre: define lowp float opacity` declares
//! `opacity`, and `#pragma maplibre: initialize lowp float opacity` brings it into
//! `main`'s scope. glslang treats an unrecognized `#pragma` as a no-op, so the
//! name looks undeclared and every such shader lights up with a false positive.
//!
//! A [`Dialect`] models one ecosystem as data, not code: an optional per-stage
//! prelude (implicit globals), an optional epilogue (a wrapper `main`), and a set
//! of [`Rule`]s that expand a directive into GLSL. Built-in presets ([`maplibre`],
//! [`shadertoy`]) are just pre-filled `Dialect`s; a project can select one, extend
//! it, or declare its own rules in `glsl-lsp.toml`. [`autodetect`] sniffs a
//! signature so an unconfigured checkout still works. Adding the next ecosystem is
//! a preset or a few config lines — never a fork of the assembler.
//!
//! The load-bearing principle: glslint does not replicate an ecosystem's runtime
//! semantics. It only has to make every symbol resolve with the right *type*, so
//! glslang stops false-erroring while still catching the author's real type errors.
//! Because a rule owns both the `define` and `initialize` templates, the
//! synthesized attribute and varying always agree by construction — the expansion
//! can't itself be the source of a type error.

use crate::assemble::Stage;

/// One directive-expansion rule. Matches `#pragma <ns>: <verb> <args…>` and
/// rewrites that single line, in place, into the GLSL declarations named by the
/// stage-appropriate template. Whitespace tokens after the verb bind positionally
/// to `args`; the template substitutes each `{arg}` (so `u_{name}` becomes
/// `u_opacity`). A rule with `verb: None` matches on the namespace alone and binds
/// every token to `args`.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The pragma namespace, e.g. `"maplibre"` for `#pragma maplibre: …`.
    pub ns: String,
    /// The first token after the namespace (`"define"`), or `None` to match any.
    pub verb: Option<String>,
    /// Names bound, in order, to the remaining whitespace tokens.
    pub args: Vec<String>,
    /// Expansion when the shader is a vertex stage.
    pub vertex: Option<String>,
    /// Expansion when the shader is a fragment stage.
    pub fragment: Option<String>,
    /// Expansion when the shader is a compute stage.
    pub compute: Option<String>,
    /// Expansion used when the stage-specific template is absent.
    pub any: Option<String>,
}

impl Rule {
    /// The template for `stage`, falling back to `any`.
    fn template(&self, stage: Stage) -> Option<&str> {
        let specific = match stage {
            Stage::Vertex => &self.vertex,
            Stage::Fragment => &self.fragment,
            Stage::Compute => &self.compute,
        };
        specific.as_deref().or(self.any.as_deref())
    }
}

/// An ecosystem's shader conventions, modeled as data. Everything is optional: a
/// pure-prelude dialect (shadertoy) has no rules; a pure-rule dialect (maplibre)
/// has no prelude.
#[derive(Debug, Clone, Default)]
pub struct Dialect {
    /// Identifier used by `preset = "…"`. Read in tests and reserved for an
    /// info-level "dialect applied" diagnostic once the LSP grows one.
    #[allow(dead_code)]
    pub name: String,
    /// Declarations prepended for a vertex stage (after `#version`/precision).
    pub prelude_vertex: Option<String>,
    /// Declarations prepended for a fragment stage.
    pub prelude_fragment: Option<String>,
    /// Declarations prepended for any stage without a stage-specific prelude.
    pub prelude_any: Option<String>,
    /// Extra prelude appended after the stage prelude for *every* stage. Carries a
    /// project's `[dialect].prelude` so it composes with a preset's own prelude.
    pub prelude_extra: Option<String>,
    /// A wrapper appended after the body (e.g. a `main` that calls `mainImage`),
    /// used only when the source has no `main` of its own.
    pub epilogue: Option<String>,
    /// Directive-expansion rules.
    pub rules: Vec<Rule>,
    /// Whether the deck.gl builtin prelude still applies. A non-deck dialect
    /// (maplibre, shadertoy) sets this false so `assemble` skips the deck stubs.
    pub deck: bool,
}

impl Dialect {
    /// The prelude for `stage`, falling back to `prelude_any`.
    pub fn prelude(&self, stage: Stage) -> Option<&str> {
        let specific = match stage {
            Stage::Vertex => &self.prelude_vertex,
            Stage::Fragment => &self.prelude_fragment,
            Stage::Compute => &None,
        };
        specific.as_deref().or(self.prelude_any.as_deref())
    }

    /// Try to expand one source `line` under `stage`. Returns the replacement
    /// lines when a rule matches (possibly empty — a fragment `initialize` under
    /// the default branch expands to nothing), or `None` to keep the line verbatim.
    pub fn expand_line(&self, line: &str, stage: Stage) -> Option<Vec<String>> {
        let (ns, rest) = parse_pragma(line)?;
        for rule in &self.rules {
            if rule.ns != ns {
                continue;
            }
            let mut tokens = rest.split_whitespace();
            if let Some(verb) = &rule.verb {
                match tokens.next() {
                    Some(t) if t == verb => {}
                    _ => continue,
                }
            }
            let values: Vec<&str> = tokens.collect();
            if values.len() != rule.args.len() {
                // Namespace matched but the shape didn't — a malformed pragma.
                // Keep it verbatim rather than silently mis-expanding.
                continue;
            }
            let template = rule.template(stage)?;
            return Some(render(template, &rule.args, &values));
        }
        None
    }
}

/// Split `#pragma <ns>: <rest>` into `(ns, rest)`. Tolerant of whitespace around
/// the colon (`#pragma maplibre :`), and returns `None` for any line that isn't a
/// namespaced pragma.
fn parse_pragma(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let after = trimmed.strip_prefix("#pragma")?;
    // Require a separator so `#pragmatic` can't match.
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let (ns, rest) = after.split_once(':')?;
    let ns = ns.trim();
    if ns.is_empty() {
        return None;
    }
    Some((ns.to_string(), rest))
}

/// Substitute each `{arg}` in `template` with its captured value, then split into
/// lines. Longest-named args are replaced first so `{name}` can't clobber a
/// hypothetical `{namespace}`.
fn render(template: &str, args: &[String], values: &[&str]) -> Vec<String> {
    let mut order: Vec<usize> = (0..args.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(args[i].len()));
    let mut out = template.to_string();
    for i in order {
        out = out.replace(&format!("{{{}}}", args[i]), values[i]);
    }
    out.lines().map(str::to_string).collect()
}

/// Whether `source` already declares its own entry point. Used to decide if a
/// dialect's epilogue wrapper should be appended.
pub fn has_main(source: &str) -> bool {
    source
        .lines()
        .any(|l| l.replace(char::is_whitespace, "").contains("voidmain("))
}

/// A project's dialect selection, assembled from `[dialect]` in `glsl-lsp.toml`.
/// Kept separate from a concrete [`Dialect`] because auto-detection needs the
/// shader source, which isn't known until assembly.
#[derive(Debug, Clone)]
pub struct Preference {
    /// A named preset that replaces auto-detection as the base dialect.
    pub preset: Option<String>,
    /// Sniff the source for a dialect when no `preset` is set.
    pub auto: bool,
    /// Project-local rules, applied ahead of the base dialect's own.
    pub custom_rules: Vec<Rule>,
    /// Project-local prelude, composed with the base dialect's prelude.
    pub custom_prelude: Option<String>,
}

impl Default for Preference {
    fn default() -> Self {
        Preference {
            preset: None,
            auto: true,
            custom_rules: Vec::new(),
            custom_prelude: None,
        }
    }
}

/// Resolve a concrete [`Dialect`] for `source`: the base is the named preset, else
/// the auto-detected dialect, else none; project-local rules and prelude are then
/// layered on top (rules take precedence, prelude composes). Returns `None` when
/// nothing applies, so `assemble` does no dialect work.
pub fn resolve(pref: &Preference, source: &str) -> Option<Dialect> {
    let base = match pref.preset.as_deref() {
        Some(name) => by_name(name),
        None if pref.auto => autodetect(source),
        None => None,
    };

    let has_custom = !pref.custom_rules.is_empty() || pref.custom_prelude.is_some();
    if base.is_none() && !has_custom {
        return None;
    }

    let mut d = base.unwrap_or_else(|| Dialect {
        name: "custom".into(),
        deck: false,
        ..Default::default()
    });
    if !pref.custom_rules.is_empty() {
        // Project rules first: `expand_line` takes the first matching rule, so a
        // project can override a preset's rule for the same namespace/verb.
        let mut rules = pref.custom_rules.clone();
        rules.extend(std::mem::take(&mut d.rules));
        d.rules = rules;
    }
    if pref.custom_prelude.is_some() {
        d.prelude_extra = pref.custom_prelude.clone();
    }
    Some(d)
}

/// Look up a built-in preset by name. `"none"`/`"raw"` disable dialect handling.
pub fn by_name(name: &str) -> Option<Dialect> {
    match name {
        "maplibre" | "mapbox" => Some(maplibre()),
        "shadertoy" => Some(shadertoy()),
        "none" | "raw" => Some(Dialect {
            name: "none".into(),
            deck: false,
            ..Default::default()
        }),
        _ => None,
    }
}

/// Pick a dialect from `source`'s tell-tale signatures. Only fires for
/// unmistakable markers, so a plain deck/luma shader is never misclassified.
pub fn autodetect(source: &str) -> Option<Dialect> {
    if source.contains("#pragma maplibre:") || source.contains("#pragma mapbox:") {
        return Some(maplibre());
    }
    // ShaderToy's entry point is distinctive; the `void main` it lacks is supplied
    // by the epilogue.
    if source
        .replace(char::is_whitespace, "")
        .contains("voidmainImage(")
    {
        return Some(shadertoy());
    }
    None
}

/// The maplibre-gl-js / mapbox-gl-js `#pragma <ns>: define|initialize` DSL.
///
/// Expansion mirrors what maplibre's own `shaders.ts` emits: a `define` at global
/// scope declares the property (as an attribute+varying in the data-driven branch,
/// a uniform otherwise), and an `initialize` inside `main` binds it. glslint never
/// defines `HAS_UNIFORM_u_*`, so glslang's preprocessor takes the data-driven
/// branch — the one that exercises the most symbols. A project that wants the
/// uniform branch checked instead can `#define HAS_UNIFORM_u_<name>` via a prelude.
pub fn maplibre() -> Dialect {
    let mut rules = Vec::new();
    for ns in ["maplibre", "mapbox"] {
        rules.push(Rule {
            ns: ns.to_string(),
            verb: Some("define".into()),
            args: vec!["prec".into(), "type".into(), "name".into()],
            vertex: Some(
                "#ifndef HAS_UNIFORM_u_{name}\n\
                 uniform lowp float u_{name}_t;\n\
                 in {prec} {type} a_{name};\n\
                 out {prec} {type} {name};\n\
                 #else\n\
                 uniform {prec} {type} u_{name};\n\
                 #endif"
                    .into(),
            ),
            fragment: Some(
                "#ifndef HAS_UNIFORM_u_{name}\n\
                 uniform lowp float u_{name}_t;\n\
                 in {prec} {type} {name};\n\
                 #else\n\
                 uniform {prec} {type} u_{name};\n\
                 #endif"
                    .into(),
            ),
            compute: None,
            any: None,
        });
        rules.push(Rule {
            ns: ns.to_string(),
            verb: Some("initialize".into()),
            args: vec!["prec".into(), "type".into(), "name".into()],
            vertex: Some(
                "#ifndef HAS_UNIFORM_u_{name}\n\
                 {name} = a_{name};\n\
                 #else\n\
                 {prec} {type} {name} = u_{name};\n\
                 #endif"
                    .into(),
            ),
            fragment: Some(
                "#ifdef HAS_UNIFORM_u_{name}\n\
                 {prec} {type} {name} = u_{name};\n\
                 #endif"
                    .into(),
            ),
            compute: None,
            any: None,
        });
    }
    Dialect {
        name: "maplibre".into(),
        deck: false,
        rules,
        ..Default::default()
    }
}

/// ShaderToy: a fragment-only environment with a fixed set of implicit uniforms
/// and a `mainImage` entry point instead of `main`. A pure-prelude dialect — the
/// prelude declares the uniforms, and the epilogue supplies the `main` that drives
/// `mainImage` when the source doesn't define one itself.
pub fn shadertoy() -> Dialect {
    Dialect {
        name: "shadertoy".into(),
        deck: false,
        prelude_fragment: Some(
            "// glslint ShaderToy prelude: implicit uniforms\n\
             uniform vec3 iResolution;\n\
             uniform float iTime;\n\
             uniform float iTimeDelta;\n\
             uniform float iFrameRate;\n\
             uniform int iFrame;\n\
             uniform vec4 iMouse;\n\
             uniform vec4 iDate;\n\
             uniform float iSampleRate;\n\
             uniform vec3 iChannelResolution[4];\n\
             uniform float iChannelTime[4];\n\
             uniform sampler2D iChannel0;\n\
             uniform sampler2D iChannel1;\n\
             uniform sampler2D iChannel2;\n\
             uniform sampler2D iChannel3;\n\
             out vec4 glslint_fragColor;"
                .into(),
        ),
        epilogue: Some("void main() { mainImage(glslint_fragColor, gl_FragCoord.xy); }".into()),
        ..Default::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: unwrap IS the assertion
mod tests {
    use super::*;

    #[test]
    fn parse_pragma_splits_namespace_and_body() {
        assert_eq!(
            parse_pragma("#pragma maplibre: define lowp float opacity"),
            Some(("maplibre".to_string(), " define lowp float opacity"))
        );
        // Indentation and whitespace around the colon are tolerated.
        assert_eq!(
            parse_pragma("    #pragma glslify : foo").map(|(n, _)| n),
            Some("glslify".to_string())
        );
        // Not a namespaced pragma.
        assert_eq!(parse_pragma("#version 300 es"), None);
        assert_eq!(parse_pragma("#pragma optimize(on)"), None);
        // Must have a separator after `#pragma`.
        assert_eq!(parse_pragma("#pragmatic: x"), None);
    }

    #[test]
    fn maplibre_define_expands_per_stage() {
        let d = maplibre();
        let v = d
            .expand_line("#pragma maplibre: define lowp float opacity", Stage::Vertex)
            .unwrap();
        // Vertex declares the attribute and the varying named after the property.
        assert!(v.iter().any(|l| l.contains("in lowp float a_opacity;")));
        assert!(v.iter().any(|l| l.contains("out lowp float opacity;")));

        let f = d
            .expand_line(
                "#pragma maplibre: define lowp float opacity",
                Stage::Fragment,
            )
            .unwrap();
        // Fragment receives it as a plain varying, no attribute.
        assert!(f.iter().any(|l| l.contains("in lowp float opacity;")));
        assert!(!f.iter().any(|l| l.contains("a_opacity")));
    }

    #[test]
    fn maplibre_initialize_binds_the_name() {
        let d = maplibre();
        let v = d
            .expand_line(
                "    #pragma maplibre: initialize lowp float opacity",
                Stage::Vertex,
            )
            .unwrap();
        assert!(v.iter().any(|l| l.contains("opacity = a_opacity;")));
    }

    #[test]
    fn mapbox_namespace_uses_the_same_rules() {
        let v = maplibre()
            .expand_line("#pragma mapbox: define highp vec4 color", Stage::Vertex)
            .unwrap();
        assert!(v.iter().any(|l| l.contains("out highp vec4 color;")));
    }

    #[test]
    fn non_matching_lines_are_left_verbatim() {
        let d = maplibre();
        assert!(
            d.expand_line("uniform vec2 u_translation;", Stage::Vertex)
                .is_none()
        );
        // A namespaced pragma whose verb we don't know is not our business.
        assert!(
            d.expand_line("#pragma maplibre: whoknows x", Stage::Vertex)
                .is_none()
        );
        // Wrong token count for a known verb → left verbatim.
        assert!(
            d.expand_line("#pragma maplibre: define float x", Stage::Vertex)
                .is_none()
        );
    }

    #[test]
    fn render_substitutes_all_occurrences() {
        let out = render(
            "uniform lowp float u_{name}_t;\nin {prec} {type} a_{name};",
            &["prec".into(), "type".into(), "name".into()],
            &["lowp", "float", "opacity"],
        );
        assert_eq!(out[0], "uniform lowp float u_opacity_t;");
        assert_eq!(out[1], "in lowp float a_opacity;");
    }

    #[test]
    fn autodetect_recognizes_maplibre_and_shadertoy() {
        assert_eq!(
            autodetect("#pragma maplibre: define lowp float opacity\n").map(|d| d.name),
            Some("maplibre".to_string())
        );
        assert_eq!(
            autodetect("void mainImage( out vec4 c, in vec2 p ) {}\n").map(|d| d.name),
            Some("shadertoy".to_string())
        );
        assert_eq!(
            autodetect("out vec4 c;\nvoid main(){}\n").map(|d| d.name),
            None
        );
    }

    #[test]
    fn has_main_detects_the_entry_point() {
        assert!(has_main("void main() { }"));
        assert!(has_main("void  main(){}"));
        assert!(!has_main("void mainImage(out vec4 c, in vec2 p){}"));
    }
}
