//! A minimal LSP server: on open/change/save, assemble + validate the document
//! and publish diagnostics. No completion/hover yet — diagnostics are the value.

use crate::check::check_source;
use crate::diagnostics::{Diag, Severity};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

/// How long to wait for typing to settle before validating. A burst of keystrokes
/// thus triggers one glslang run, not one per character.
const DEBOUNCE: Duration = Duration::from_millis(200);

pub async fn run() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: Mutex::new(HashMap::new()),
        generations: Mutex::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, String>>,
    /// Monotonic counter per document. Each edit bumps it; a refresh publishes
    /// only if its captured value is still current, so a slow check can't clobber
    /// a newer one (out-of-order publishes).
    generations: Mutex<HashMap<Url, u64>>,
}

impl Backend {
    async fn refresh(&self, uri: Url, text: String) {
        // Record the text and claim a generation for this edit.
        let generation = {
            let mut docs = self.docs.lock().unwrap();
            docs.insert(uri.clone(), text.clone());
            let mut gens = self.generations.lock().unwrap();
            let g = gens.entry(uri.clone()).or_insert(0);
            *g += 1;
            *g
        };

        // Debounce: if a newer edit lands during the wait, drop this one — that
        // edit's own refresh will publish.
        tokio::time::sleep(DEBOUNCE).await;
        if self.superseded(&uri, generation) {
            return;
        }
        let Ok(path) = uri.to_file_path() else { return };

        // check_source spawns a glslang subprocess and reads module files; run it
        // off the async runtime so it never blocks other LSP requests.
        let (check_path, check_text) = (path.clone(), text.clone());
        let diags = match tokio::task::spawn_blocking(move || check_source(&check_path, &check_text)).await {
            Ok(d) => d,
            Err(_) => return, // the blocking task panicked or was cancelled
        };
        // A newer edit may have arrived while we were checking — let it publish.
        if self.superseded(&uri, generation) {
            return;
        }

        // Only surface diagnostics that belong to *this* document; errors that map
        // into an injected module surface when that file is opened/checked.
        let lines: Vec<&str> = text.lines().collect();
        let lsp_diags: Vec<Diagnostic> = diags
            .iter()
            .filter(|d| same_path(&d.path, &path))
            .map(|d| to_lsp(d, lines.get(d.line.saturating_sub(1) as usize).copied()))
            .collect();

        self.client.publish_diagnostics(uri, lsp_diags, None).await;
    }

    /// True if a newer edit has superseded `generation` for `uri` (or the document
    /// was closed), meaning this refresh should not publish.
    fn superseded(&self, uri: &Url, generation: u64) -> bool {
        self.generations.lock().unwrap().get(uri).copied() != Some(generation)
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "glslint".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "glslint LSP ready")
            .await;
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        self.refresh(p.text_document.uri, p.text_document.text).await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        // FULL sync: the last change event carries the whole document.
        if let Some(change) = p.content_changes.into_iter().last() {
            self.refresh(p.text_document.uri, change.text).await;
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        let uri = p.text_document.uri;
        let text = p
            .text
            .or_else(|| self.docs.lock().unwrap().get(&uri).cloned())
            .unwrap_or_default();
        self.refresh(uri, text).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        let uri = p.text_document.uri;
        self.docs.lock().unwrap().remove(&uri);
        // Drop the generation too, so any in-flight refresh for this doc bails.
        self.generations.lock().unwrap().remove(&uri);
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

fn to_lsp(d: &Diag, line_text: Option<&str>) -> Diagnostic {
    let line = d.line.saturating_sub(1);
    let (start, end) = utf16_range(line_text, d.col, d.len);
    Diagnostic {
        range: Range::new(Position::new(line, start), Position::new(line, end)),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        source: Some(format!("glslint/{}", d.source)),
        message: d.message.clone(),
        ..Default::default()
    }
}

/// Convert a 1-based *char* column and *char* length (what `Diag` carries) into a
/// UTF-16 code-unit range — the encoding LSP positions use by default — clamped to
/// the line so the end never runs past EOL. For ASCII (i.e. ~every GLSL line) this
/// is the identity; it only matters when a multibyte char precedes the token.
fn utf16_range(line_text: Option<&str>, col: u32, len: u32) -> (u32, u32) {
    let start_char = col.saturating_sub(1) as usize;
    let span_char = len.max(1) as usize;
    let Some(text) = line_text else {
        // No line text (shouldn't happen) — fall back to char offsets (correct for ASCII).
        return (start_char as u32, (start_char + span_char) as u32);
    };
    let u16_upto = |n: usize| -> u32 { text.chars().take(n).map(|c| c.len_utf16() as u32).sum() };
    let total = u16_upto(text.chars().count());
    let start = u16_upto(start_char).min(total);
    let end = u16_upto(start_char + span_char).min(total).max(start);
    (start, end)
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
