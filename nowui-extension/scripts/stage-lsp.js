// Copies the `nowui-lsp` release binary for the *current* platform into
// `nowui-extension/bin/`, so `vsce package --target <platform>` bundles it
// into the .vsix. Run this once per platform (in a matching OS/arch build
// environment, e.g. one leg of a CI matrix) right before packaging that
// platform's target — see the `package:<platform>` scripts in package.json
// and src/extension.ts's `resolveServerPath` for how the shipped binary is
// found at runtime.

const fs = require("fs");
const path = require("path");

const exeName = process.platform === "win32" ? "nowui-lsp.exe" : "nowui-lsp";
const src = path.join(__dirname, "..", "..", "target", "release", exeName);
const destDir = path.join(__dirname, "..", "bin");
const dest = path.join(destDir, exeName);

if (!fs.existsSync(src)) {
    console.error(`nowui-lsp release binary not found at ${src}`);
    console.error('Build it first: cargo build --release -p nowui-lsp');
    process.exit(1);
}

fs.mkdirSync(destDir, { recursive: true });
fs.copyFileSync(src, dest);
if (process.platform !== "win32") {
    fs.chmodSync(dest, 0o755);
}

console.log(`Staged ${src} -> ${dest}`);
