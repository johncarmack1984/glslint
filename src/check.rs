//! Drive naga over an assembled unit and translate its spans back to the
//! original files via the line map.

use crate::assemble::{self, Assembled};
use crate::config::Config;
use crate::diagnostics::{Diag, Severity};
use crate::lints;
use naga::front::glsl::{Frontend, Options};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use std::path::Path;

pub fn check_file(path: &Path) -> anyhow::Result<Vec<Diag>> {
    let source = std::fs::read_to_string(path)?;
    Ok(check_source(path, &source))
}

pub fn check_source(path: &Path, source: &str) -> Vec<Diag> {
    let config = Config::resolve_for(path);
    let assembled = assemble::assemble(path, source, &config);

    let mut diags = check_assembled(&assembled);
    diags.extend(lints::run_lints(path, source));
    diags.sort_by(|a, b| {
        (a.path.as_path(), a.line, a.col).cmp(&(b.path.as_path(), b.line, b.col))
    });
    diags
}

fn check_assembled(a: &Assembled) -> Vec<Diag> {
    let mut out = Vec::new();
    let mut frontend = Frontend::default();
    let options = Options::from(a.stage);

    let module = match frontend.parse(&options, &a.source) {
        Ok(m) => m,
        Err(errs) => {
            for e in &errs.errors {
                let loc = e.meta.location(&a.source);
                if let Some(d) = map_diag(a, loc.line_number, loc.line_position, loc.length, Severity::Error, format!("{}", e.kind), "parse") {
                    out.push(d);
                }
            }
            // Never leave a real parse failure silent (spans can land on
            // synthetic lines we drop).
            if out.is_empty() {
                if let Some(first) = errs.errors.first() {
                    out.push(fallback(a, format!("{}", first.kind), "parse"));
                }
            }
            return out;
        }
    };

    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    if let Err(err) = validator.validate(&module) {
        let message = format!("{err}");
        let mut placed = false;
        for (span, label) in err.spans() {
            let loc = span.location(&a.source);
            let msg = if label.is_empty() { message.clone() } else { format!("{message} ({label})") };
            if let Some(d) = map_diag(a, loc.line_number, loc.line_position, loc.length, Severity::Error, msg, "validate") {
                out.push(d);
                placed = true;
            }
        }
        if !placed {
            out.push(fallback(a, message, "validate"));
        }
    }

    out
}

/// Translate a 1-based location in the assembled source to a diagnostic against
/// the original file. Returns `None` when the line is synthetic/injected.
fn map_diag(a: &Assembled, asm_line: u32, asm_col: u32, len: u32, severity: Severity, message: String, source: &'static str) -> Option<Diag> {
    let idx = asm_line.checked_sub(1)? as usize;
    let loc = a.map.get(idx)?.as_ref()?;
    Some(Diag {
        path: loc.path.clone(),
        line: loc.line,
        col: asm_col.max(1),
        len: len.max(1),
        severity,
        message,
        source,
    })
}

/// A diagnostic pinned to line 1 of the target when we couldn't map a span.
fn fallback(a: &Assembled, message: String, source: &'static str) -> Diag {
    Diag {
        path: a.target.clone(),
        line: 1,
        col: 1,
        len: 1,
        severity: Severity::Error,
        message,
        source,
    }
}
