//! glslint — a luma.gl/deck.gl-aware GLSL checker.
//!
//! Stock GLSL tools choke on shaders that reference module-injected symbols
//! (`wind.*`, `project_position_to_clipspace`). glslint assembles the modules +
//! deck builtins into a complete `#version 300 es` unit, validates it with the
//! Khronos glslangValidator reference compiler, and maps diagnostics back to the
//! original files.
//!
//!   glslint check FILE...   # one-shot validation (exit 1 on errors)
//!   glslint lsp             # language server over stdio (publishDiagnostics)

mod assemble;
mod check;
mod config;
mod deck;
mod diagnostics;
mod drift;
mod lints;
mod lsp;
mod symbols;

use diagnostics::{print_diag, Severity};
use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lsp") => lsp::run().await,
        Some("check") => {
            run_check(&args[1..]);
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("glslint {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: glslint <check FILE... | lsp>");
            std::process::exit(2);
        }
    }
}

fn run_check(files: &[String]) {
    if files.is_empty() {
        eprintln!("glslint check: no files given");
        std::process::exit(2);
    }

    let mut errors = 0usize;
    let mut warnings = 0usize;

    for f in files {
        match check::check_file(Path::new(f)) {
            Ok(diags) => {
                for d in &diags {
                    print_diag(d);
                    match d.severity {
                        Severity::Error => errors += 1,
                        Severity::Warning => warnings += 1,
                    }
                }
            }
            Err(e) => {
                eprintln!("{f}: {e}");
                errors += 1;
            }
        }
    }

    eprintln!("\nglslint: {errors} error(s), {warnings} warning(s)");
    if errors > 0 {
        std::process::exit(1);
    }
}
