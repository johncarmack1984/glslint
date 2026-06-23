//! The single diagnostic type every stage produces, plus terminal rendering.

use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
    /// ANSI color for the severity label (red / yellow).
    fn color(self) -> &'static str {
        match self {
            Severity::Error => "\x1b[31m",
            Severity::Warning => "\x1b[33m",
        }
    }
}

/// A diagnostic resolved back to its *original* source file and 1-based
/// line/column — never to the assembled translation unit.
#[derive(Debug, Clone)]
pub struct Diag {
    /// File the diagnostic belongs to (the edited shader, or an injected
    /// module/prelude file when the error originates there).
    pub path: PathBuf,
    /// 1-based line within `path`.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
    /// Length of the highlighted span, in chars (clamped to >= 1).
    pub len: u32,
    pub severity: Severity,
    pub message: String,
    /// Which stage emitted it: "glslang" | "lint" | "glslint" (the last for
    /// tool-level failures, e.g. a missing validator).
    pub source: &'static str,
}

/// Print one diagnostic in `path:line:col: severity: message [source]` form,
/// with color unless `NO_COLOR` is set or output isn't a tty-ish stream.
pub fn print_diag(d: &Diag) {
    let color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let (c0, c1, dim, dim0) = if color {
        (d.severity.color(), "\x1b[0m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    println!(
        "{}:{}:{}: {c0}{}{c1}: {} {dim}[glslint/{}]{dim0}",
        d.path.display(),
        d.line,
        d.col,
        d.severity.label(),
        d.message,
        d.source,
    );
}
