# NowUI for VS Code

Syntax highlighting and parse-error diagnostics for `.nowui` files, backed by
`nowui-lsp` — a real language server (see `../nowui-lsp`), not a static
TextMate grammar. Highlighting comes from `textDocument/semanticTokens/full`;
diagnostics come from the same parser (`nowui-syntax`) the rest of the
toolkit uses.

## Setup (development)

1. Build the language server once: `cargo build -p nowui-lsp` from the
   repository root. The extension auto-detects
   `target/debug/nowui-lsp[.exe]` (or `target/release/...`) under the first
   open workspace folder — no configuration needed if you open this repo
   itself in VS Code.
2. In this directory: `npm install && npm run compile`.
3. Press F5 (or run the "Launch NowUI Extension" configuration) to open an
   Extension Development Host with the extension loaded. Open any `.nowui`
   file to see it highlighted.

If you're using the extension against a different NowUI checkout, or a
`cargo install`ed server, set `nowui.serverPath` in your VS Code settings to
the executable's path (or leave it empty to fall back to `PATH`).

## What's highlighted

Token types (see `nowui-lsp/src/tokenizer.rs`): comments, keywords
(`if`/`else`/`for`/`in`/`true`/`false`/`layout`), backtick and quoted
strings, numbers, widget/layout names (`Text`, `Menu`, a custom layout's own
name, ...), dotted `state.*`/loop-variable paths, and style/binding/arg
keys. Punctuation and `${...}` interpolation inside a backtick aren't
separately colored — see the tokenizer's module docs for the full list of
deliberate simplifications.
