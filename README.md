# NowUI

A file-based, retained-mode UI toolkit for Rust with a custom Tailwind-flavored
syntax. CPU-rasterized with `tiny-skia`, presented via `softbuffer` + `winit`.

> Status: scaffold. The parser, semantic pass, layout solver, paint walk, and
> window harness are wired end to end and render the login example as colored
> boxes. **Text is a placeholder** (faint baseline bars) until the `cosmic-text`
> milestone — see step 7 below.

## Workspace layout

```
nowui/
├── nowui-syntax/    lexer + chumsky parser -> AST      (no core, no render)
├── nowui-core/      arena, Style, solver, paint walk,
│                    Painter trait                       (no chumsky, no tiny-skia)
├── nowui-render/    tiny-skia Painter + softbuffer bridge
├── nowui-runtime/   semantic pass + winit app (binary `nowui`)
└── examples/login.nowui
```

The dependency arrows point one way only: `nowui-core` knows about neither the
parser nor the renderer, so the model stays testable in isolation and the
backend can be swapped.

## Build & run

Requires a Rust toolchain (stable). This scaffold has not been compiled in the
authoring environment — expect to run `cargo build` once and resolve any
version drift (see the winit note below).

```bash
cargo build
cargo test                 # parser + solver unit tests, no window needed
cargo run -p nowui-runtime -- examples/login.nowui App
```

`cargo test` is the fast inner loop and needs no display. It covers the parser
(against `examples/login.nowui`) and the layout solver (hand-built arenas).

## The language

```
layout: Login(onSubmit) w-[fill] h-[fill] {
  Card bg-color-[#ffffff] p-[32px] {
    Text `Login Form` align-text-[center]
    TextInput `` `Enter Username` w-[fill] {value: state.username}
    Button `SIGN IN` bg-color-[#2680d4] {onClick: onSubmit}
  }
}
```

* **Colon-delimited, brace-nested** — not whitespace-sensitive.
* **Widget line order** is fixed: `Kind  args=...  \`strings\`  style-[...]  { bindings }  { children }`.
* **Backtick string literals** carry `${var}` interpolation, resolved at runtime.
* **Styles** are Tailwind-style `key-[value]` tokens (or bare flags like `grid`);
  the parser keeps them generic and the semantic pass resolves them into `Style`.
* **`layout` is a reusable, parameterized widget** — `Name(params)` defines,
  `Name arg=value` uses. Custom widgets and layouts are the same mechanism,
  expanded before layout solving.

Sizing values: `w-[fill]`, `w-[fill-2]` (weight 2), `w-[hug]`, `w-[440px]`.

## Build order (each step runnable/testable before the next)

1. Parser green — `cargo test -p nowui-syntax`.
2. Core model + solver green on hand-built arenas — `cargo test -p nowui-core`.
3. Boxes on screen — `cargo run -p nowui-runtime` (this scaffold's current state).
4. **Text** — add `cosmic-text` to `nowui-render`, implement `draw_text` by
   shaping the string and blitting glyph alpha bitmaps onto the pixmap. Replace
   the placeholder in `nowui-render/src/lib.rs`. Wire real `measure_text` so
   `Hug` text nodes size correctly.
5. Input + focus — dispatch `onClick`, toggle `Checkbox`, then tackle
   `TextInput` (cursor, selection, IME) last.
6. Per-layer pixmap caching — re-rasterize only dirty layers, then composite.

## Known simplifications

* `draw_text` is a placeholder bar; no real fonts yet (step 4).
* The solver is a compact two-pass flex approximation, not full flexbox
  (no min/max, wrap, or grid). Swap the internals of `nowui-core/src/layout.rs`
  for `taffy` if you want complete semantics without touching the arena.
* Rectangular clips only; nested clip intersection is simplified.
* `${var}` and `state.*` paths parse and are stored but are not yet bound to a
  live state object — that's the reactivity layer (after step 5).

## winit version note

`nowui-runtime` targets **winit 0.29** (`ApplicationHandler` + `run_app`,
`resumed(&ActiveEventLoop)`). winit 0.30 renames/reshapes some callbacks
(`can_create_surfaces`, `&dyn ActiveEventLoop`). If cargo resolves 0.30 and the
build fails, align the method signatures in `nowui-runtime/src/app.rs` with the
docs for the resolved version — the logic is unchanged. Pin explicitly in
`Cargo.toml` (`[workspace.dependencies] winit = "=0.29.x"`) to avoid surprises.
