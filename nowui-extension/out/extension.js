"use strict";
// VS Code extension entry point: registers the `nowui` language and starts
// `nowui-lsp` as a language client over stdio. All the actual highlighting
// logic (semantic tokens) and diagnostics live in the server
// (`nowui-lsp`, a Rust binary) — this file's only job is finding that
// executable and wiring up `vscode-languageclient`.
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const vscode_1 = require("vscode");
const node_1 = require("vscode-languageclient/node");
let client;
/**
 * Resolve the `nowui-lsp` executable to launch, in priority order:
 *   1. the `nowui.serverPath` setting, if set;
 *   2. `target/debug/nowui-lsp[.exe]` or `target/release/nowui-lsp[.exe]`
 *      under any open workspace folder (the common case while developing
 *      NowUI itself, right after `cargo build -p nowui-lsp`);
 *   3. bare `nowui-lsp`, resolved via `PATH` (e.g. after
 *      `cargo install --path nowui-lsp`).
 */
function resolveServerPath() {
    const configured = vscode_1.workspace.getConfiguration("nowui").get("serverPath");
    if (configured && configured.trim().length > 0) {
        return configured;
    }
    const exeName = process.platform === "win32" ? "nowui-lsp.exe" : "nowui-lsp";
    for (const folder of vscode_1.workspace.workspaceFolders ?? []) {
        for (const profile of ["debug", "release"]) {
            const candidate = path.join(folder.uri.fsPath, "target", profile, exeName);
            if (fs.existsSync(candidate)) {
                return candidate;
            }
        }
    }
    return exeName;
}
function activate(_context) {
    const command = resolveServerPath();
    const serverOptions = {
        run: { command, transport: node_1.TransportKind.stdio },
        debug: { command, transport: node_1.TransportKind.stdio },
    };
    const clientOptions = {
        documentSelector: [{ scheme: "file", language: "nowui" }],
    };
    client = new node_1.LanguageClient("nowui", "NowUI Language Server", serverOptions, clientOptions);
    client.start();
}
function deactivate() {
    return client?.stop();
}
//# sourceMappingURL=extension.js.map