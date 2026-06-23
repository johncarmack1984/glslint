//! A minimal LSP server: on open/change/save, assemble + validate the document
//! and publish diagnostics. No completion/hover yet — diagnostics are the value.

use crate::check::check_source;
use crate::config::Config;
use crate::diagnostics::{Diag, Severity};
use crate::{assemble, symbols};
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

    /// Resolve the symbol under `pos` to a hover/definition hit. Assembles the
    /// document (cheap — no glslang subprocess) and scans it for symbols. Position
    /// columns are treated as byte offsets, correct for ASCII GLSL.
    fn resolve_at(&self, uri: &Url, pos: Position) -> Option<symbols::Hit> {
        let (text, _, index) = self.index_for(uri)?;
        let line = text.lines().nth(pos.line as usize)?;
        symbols::resolve(&index, line, pos.character as usize)
    }

    /// Assemble the document and scan it for symbols. Cheap — no glslang
    /// subprocess. Returns the document text and its file path alongside the index.
    fn index_for(&self, uri: &Url) -> Option<(String, std::path::PathBuf, symbols::SymbolIndex)> {
        let text = self.docs.lock().unwrap().get(uri)?.clone();
        let path = uri.to_file_path().ok()?;
        let config = Config::resolve_for(&path);
        let assembled = assemble::assemble(&path, &text, &config);
        Some((text, path, symbols::index(&assembled)))
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
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
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

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let p = params.text_document_position_params;
        Ok(self.resolve_at(&p.text_document.uri, p.position).map(|hit| {
            let mut value = format!("```glsl\n{}\n```", hit.detail);
            if let Some(note) = hit.note {
                value.push_str(&format!("\n\n{note}"));
            }
            Hover {
                contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }),
                range: None,
            }
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let p = params.text_document_position_params;
        Ok(self.resolve_at(&p.text_document.uri, p.position).and_then(|hit| {
            let loc = hit.loc?; // builtins have no source location
            let uri = Url::from_file_path(&loc.path).ok()?;
            let line = loc.line.saturating_sub(1);
            let range = Range::new(Position::new(line, 0), Position::new(line, 0));
            Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
        }))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let p = params.text_document_position;
        let Some((text, _, index)) = self.index_for(&p.text_document.uri) else {
            return Ok(None);
        };
        let Some(line) = text.lines().nth(p.position.line as usize) else {
            return Ok(None);
        };
        let items: Vec<CompletionItem> = symbols::complete(&index, line, p.position.character as usize)
            .into_iter()
            .map(to_completion_item)
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> tower_lsp::jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let Some((text, path, index)) = self.index_for(&params.text_document.uri) else {
            return Ok(None);
        };
        let lines: Vec<&str> = text.lines().collect();
        let syms: Vec<DocumentSymbol> = symbols::document_symbols(&index, &path)
            .into_iter()
            .map(|d| to_document_symbol(d, &lines))
            .collect();
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
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

fn to_completion_item(c: symbols::Completion) -> CompletionItem {
    CompletionItem {
        label: c.label,
        kind: Some(completion_kind(c.kind)),
        detail: Some(c.detail),
        ..Default::default()
    }
}

fn completion_kind(k: symbols::SymKind) -> CompletionItemKind {
    match k {
        symbols::SymKind::Field => CompletionItemKind::FIELD,
        symbols::SymKind::Function | symbols::SymKind::Builtin => CompletionItemKind::FUNCTION,
        symbols::SymKind::Variable => CompletionItemKind::VARIABLE,
        symbols::SymKind::Block => CompletionItemKind::STRUCT,
    }
}

#[allow(deprecated)] // `DocumentSymbol::deprecated` is a required (deprecated) field
fn to_document_symbol(d: symbols::DocSym, lines: &[&str]) -> DocumentSymbol {
    let line = d.line.saturating_sub(1);
    let len = lines.get(line as usize).map(|l| l.chars().count() as u32).unwrap_or(0);
    let range = Range::new(Position::new(line, 0), Position::new(line, len));
    let children: Vec<DocumentSymbol> = d.children.into_iter().map(|c| to_document_symbol(c, lines)).collect();
    DocumentSymbol {
        name: d.name,
        detail: Some(d.detail),
        kind: symbol_kind(d.kind),
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: (!children.is_empty()).then_some(children),
    }
}

fn symbol_kind(k: symbols::SymKind) -> SymbolKind {
    match k {
        symbols::SymKind::Field => SymbolKind::FIELD,
        symbols::SymKind::Function | symbols::SymKind::Builtin => SymbolKind::FUNCTION,
        symbols::SymKind::Variable => SymbolKind::VARIABLE,
        symbols::SymKind::Block => SymbolKind::STRUCT,
    }
}
