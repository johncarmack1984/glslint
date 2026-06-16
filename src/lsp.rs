//! A minimal LSP server: on open/change/save, assemble + validate the document
//! and publish diagnostics. No completion/hover yet — diagnostics are the value.

use crate::check::check_source;
use crate::diagnostics::{Diag, Severity};
use std::collections::HashMap;
use std::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

pub async fn run() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: Mutex::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, String>>,
}

impl Backend {
    async fn refresh(&self, uri: Url, text: String) {
        {
            let mut docs = self.docs.lock().unwrap();
            docs.insert(uri.clone(), text.clone());
        }
        let Ok(path) = uri.to_file_path() else { return };

        let diags = check_source(&path, &text);
        // Only surface diagnostics that belong to *this* document; errors that
        // map into an injected module surface when that file is opened/checked.
        let lsp_diags: Vec<Diagnostic> = diags
            .iter()
            .filter(|d| same_path(&d.path, &path))
            .map(to_lsp)
            .collect();

        self.client.publish_diagnostics(uri, lsp_diags, None).await;
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
        self.docs.lock().unwrap().remove(&p.text_document.uri);
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

fn to_lsp(d: &Diag) -> Diagnostic {
    let line = d.line.saturating_sub(1);
    let ch = d.col.saturating_sub(1);
    Diagnostic {
        range: Range::new(Position::new(line, ch), Position::new(line, ch + d.len.max(1))),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        source: Some(format!("glslint/{}", d.source)),
        message: d.message.clone(),
        ..Default::default()
    }
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
