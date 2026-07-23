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
 *      NowUI itself, right after `cargo build -p nowui-lsp`) — takes
 *      priority over the bundled binary below so a dev iterating on the
 *      server gets their own build, not the one shipped in the `.vsix`;
 *   3. `bin/nowui-lsp[.exe]` inside this extension's own install directory
 *      — present only in a packaged `.vsix` that ran `npm run stage-lsp`
 *      before `vsce package` (see `scripts/stage-lsp.js` and the
 *      `package:<platform>` scripts in `package.json`); absent entirely for
 *      a plain `npm install && npm run compile` dev checkout;
 *   4. bare `nowui-lsp`, resolved via `PATH` (e.g. after
 *      `cargo install --path nowui-lsp`).
 */
function resolveServerPath(extensionPath: string): string {
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

    const bundled = path.join(extensionPath, "bin", exeName);
    if (fs.existsSync(bundled)) {
        return bundled;
    }

    return exeName;
}

export function activate(context: ExtensionContext): void {
    const command = resolveServerPath(context.extensionPath);
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
