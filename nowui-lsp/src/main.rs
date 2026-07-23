//! `nowui-lsp` — a minimal language server for `.nowui` files, providing
//! syntax highlighting via `textDocument/semanticTokens/full` (see
//! `tokenizer.rs`) and parse-error diagnostics (via `nowui_syntax::parse`,
//! the same parser the rest of the toolkit uses) via `publishDiagnostics`.
//! Talks LSP over stdio — the `nowui-extension` VS Code extension spawns
//! this binary and connects to it with `vscode-languageclient`.
//!
//! Scope (deliberately small — see `tokenizer.rs`'s module docs for what's
//! simplified): no completion, hover, go-to-definition, or incremental
//! sync (`TextDocumentSyncKind::FULL` — simplest correct option; `.nowui`
//! files are small enough that re-tokenizing the whole document on every
//! keystroke is cheap).

mod line_index;
mod tokenizer;

use std::collections::HashMap;
use std::sync::Mutex;

use line_index::LineIndex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    /// One entry per open document, keyed by URI, holding its current full
    /// text (kept in sync via `did_open`/`did_change`/`did_close`) — both
    /// `semantic_tokens_full` and diagnostics re-derive everything from
    /// this on demand rather than caching tokens/diagnostics themselves.
    documents: Mutex<HashMap<Url, String>>,
}

impl Backend {
    /// Parse `text` and publish one diagnostic per parse error, replacing
    /// whatever diagnostics this `uri` had before (an empty list clears
    /// them — required after a fix, not just to add new ones).
    async fn publish_diagnostics(&self, uri: Url, text: &str) {
        let diagnostics = match nowui_syntax::parse(text) {
            Ok(_) => Vec::new(),
            Err(errors) => {
                let line_index = LineIndex::new(text);
                errors
                    .into_iter()
                    .map(|e| {
                        let span = e.span();
                        let (start_line, start_col) = line_index.position(span.start);
                        let (end_line, end_col) = line_index.position(span.end.max(span.start + 1));
                        Diagnostic {
                            range: Range {
                                start: Position::new(start_line, start_col),
                                end: Position::new(end_line, end_col),
                            },
                            severity: Some(DiagnosticSeverity::ERROR),
                            source: Some("nowui".to_string()),
                            message: e.to_string(),
                            ..Default::default()
                        }
                    })
                    .collect()
            }
        };
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo { name: "nowui-lsp".to_string(), version: Some(env!("CARGO_PKG_VERSION").to_string()) }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend { token_types: tokenizer::TOKEN_TYPES.to_vec(), token_modifiers: vec![] },
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    range: Some(false),
                    ..Default::default()
                })),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client.log_message(MessageType::INFO, "nowui-lsp initialized").await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.documents.lock().unwrap().insert(uri.clone(), text.clone());
        self.publish_diagnostics(uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // `TextDocumentSyncKind::FULL` — the last (and only) content change
        // is always the whole new document text.
        let Some(change) = params.content_changes.into_iter().last() else { return };
        let uri = params.text_document.uri;
        self.documents.lock().unwrap().insert(uri.clone(), change.text.clone());
        self.publish_diagnostics(uri, &change.text).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.lock().unwrap().remove(&params.text_document.uri);
    }

    async fn semantic_tokens_full(&self, params: SemanticTokensParams) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(text) = self.documents.lock().unwrap().get(&uri).cloned() else {
            return Ok(None);
        };
        let tokens = tokenizer::tokenize(&text);
        let line_index = LineIndex::new(&text);
        let data = encode_tokens(&tokens, &line_index);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data })))
    }
}

/// LSP semantic tokens are delta-encoded relative to the previous token
/// (`deltaLine`/`deltaStart`, both `0` for the very first) and each must lie
/// on a single line. A token whose span crosses a line boundary (a
/// backtick/quoted string containing a literal newline — comments and `#`
/// imports never do, since both stop scanning at the first `\n`) has no
/// valid single-line representation, so it's dropped rather than encoded
/// incorrectly; a rare, disclosed gap, not a crash.
fn encode_tokens(tokens: &[tokenizer::Token], line_index: &LineIndex) -> Vec<SemanticToken> {
    let mut spans: Vec<(u32, u32, u32, u32)> = tokens // (line, start_col, length, kind)
        .iter()
        .filter_map(|t| {
            let (line, col) = line_index.position(t.start);
            let (end_line, end_col) = line_index.position(t.start + t.len);
            if end_line != line {
                return None;
            }
            Some((line, col, end_col - col, t.kind))
        })
        .collect();
    spans.sort_by_key(|&(line, col, ..)| (line, col));

    let mut result = Vec::with_capacity(spans.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for (line, col, length, kind) in spans {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 { col - prev_start } else { col };
        result.push(SemanticToken { delta_line, delta_start, length, token_type: kind, token_modifiers_bitset: 0 });
        prev_line = line;
        prev_start = col;
    }
    result
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend { client, documents: Mutex::new(HashMap::new()) });
    Server::new(stdin, stdout, socket).serve(service).await;
}
