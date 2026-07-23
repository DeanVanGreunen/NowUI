// VS Code extension entry point: registers the `nowui` language and starts
// `nowui-lsp` as a language client over stdio. All the actual highlighting
// logic (semantic tokens) and diagnostics live in the server
// (`nowui-lsp`, a Rust binary) — this file's only job is finding that
// executable and wiring up `vscode-languageclient`.

import * as fs from "fs";
import * as path from "path";
import { ExtensionContext, workspace } from "vscode";
import { LanguageClient, LanguageClientOptions, ServerOptions, TransportKind } from "vscode-languageclient/node";

let client: LanguageClient | undefined;

/**
 * Resolve the `nowui-lsp` executable to launch, in priority order:
 *   1. the `nowui.serverPath` setting, if set;
 *   2. `target/debug/nowui-lsp[.exe]` or `target/release/nowui-lsp[.exe]`
 *      under any open workspace folder (the common case while developing
 *      NowUI itself, right after `cargo build -p nowui-lsp`);
 *   3. bare `nowui-lsp`, resolved via `PATH` (e.g. after
 *      `cargo install --path nowui-lsp`).
 */
function resolveServerPath(): string {
    const configured = workspace.getConfiguration("nowui").get<string>("serverPath");
    if (configured && configured.trim().length > 0) {
        return configured;
    }

    const exeName = process.platform === "win32" ? "nowui-lsp.exe" : "nowui-lsp";
    for (const folder of workspace.workspaceFolders ?? []) {
        for (const profile of ["debug", "release"]) {
            const candidate = path.join(folder.uri.fsPath, "target", profile, exeName);
            if (fs.existsSync(candidate)) {
                return candidate;
            }
        }
    }

    return exeName;
}

export function activate(_context: ExtensionContext): void {
    const command = resolveServerPath();
    const serverOptions: ServerOptions = {
        run: { command, transport: TransportKind.stdio },
        debug: { command, transport: TransportKind.stdio },
    };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: "file", language: "nowui" }],
    };

    client = new LanguageClient("nowui", "NowUI Language Server", serverOptions, clientOptions);
    client.start();
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
