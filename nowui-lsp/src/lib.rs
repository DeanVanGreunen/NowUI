//! Library surface for `nowui-lsp`'s own tokenizer/line-index, for reuse by
//! in-process consumers that want `.nowui` syntax highlighting without
//! speaking real LSP/JSON-RPC — currently `nowui-designer`'s code editor
//! (see its own module doc for why: it's already native Rust sitting right
//! next to `nowui-syntax`, so calling `tokenize`/`LineIndex` directly is
//! simpler than a stdio subprocess round-trip for a highlighter that could
//! just be a function call). The `nowui-lsp` binary itself (`main.rs`) uses
//! these same modules for the real `textDocument/semanticTokens/full` path.

pub mod line_index;
pub mod tokenizer;

pub use line_index::LineIndex;
pub use tokenizer::{tokenize, Token, TOKEN_TYPES};
