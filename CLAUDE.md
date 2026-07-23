# CLAUDE.md

Standing instructions for working in the NowUI repository. Read this before making changes.

---

## Project Overview

NowUI is a file-based, retained-mode UI toolkit for Rust with a custom Tailwind-flavored syntax.
UIs are described in `.nowui` files, parsed to an AST, expanded into a flat node arena, laid out
with a two-pass layout solver, and CPU-rasterized to a window. The reference target used
throughout development is a login screen: dark top bar, blue field, centered white card with
username/password inputs and a SIGN IN button (`examples/counter-app/src/login.nowui`).

Pipeline, end to end:

```text
.nowui file --chumsky parser--> AST --semantic pass--> node arena
   --layout solver (2-pass)--> computed rects --paint walk--> Painter calls
   --tiny-skia raster--> Pixmap --softbuffer--> window pixels
```

Two properties are load-bearing and shape everything else in this document:

- **Retained, not immediate.** The arena persists across frames. A redraw re-walks the existing
  tree and re-paints it; it does not rebuild the tree from scratch, except where `if`/`for`
  dynamic regions explicitly re-expand a subtree because the state they depend on changed.
- **Event-driven, not a game loop.** The window uses `ControlFlow::Wait` and renders only when
  dirty (a state mutation, a hover, a resize, an in-flight transition). There is **no continuous
  animation loop** anywhere in this engine — don't add one to solve a redraw problem; see the
  `ControlFlow` runtime gotcha under "Internal Libraries and Dependencies" below.

---

## Internal Libraries and Dependencies

### Third-party crates (do not change without reason)

- **`chumsky`** (0.9) — parser combinators; builds the `.nowui` AST.
- **`tiny-skia`** (0.11) — CPU rasterizer. Has **no text support** — glyphs come from `cosmic-text`.
- **`cosmic-text`** (0.12) — text shaping/layout/rasterization, feeds glyphs into tiny-skia.
- **`winit`** (**0.30**) — windowing + event loop.
- **`softbuffer`** (0.4) — presents the rasterized pixmap to the OS window.
- **`syn` / `quote` / `proc-macro2`** (2 / 1 / 1) — power the `#[derive(NowUiState)]` proc-macro.

**winit's version is load-bearing.** The app harness uses `ApplicationHandler` + `run_app`, which
live in `winit::application` / `winit::event_loop` as of **0.30** — they do not exist on 0.29 or
earlier (that's the old closure-based API). Keep `winit = "0.30"` in `[workspace.dependencies]`.
If a build fails with `unresolved import winit::application`, the version was downgraded — fix
the pin, not the code.

### Internal crates and what each one owns

- **`nowui-syntax`** — the chumsky parser and AST. No `nowui-core` dependency, no render
  dependency. Owns: widget grammar, style-token grammar, `#` import statements, the `if`/`for`
  control-flow grammar and its `Expr` sub-language (dotted paths, comparisons, `&&`/`||`/`!`).
- **`nowui-core`** — the node arena, `Style`, Tailwind design tokens, geometry, the two-pass
  layout solver, the paint walk, the `Painter` trait, and the reactivity interface
  (`NowUiState` trait, `StateValue`, `Event`). Pure model — no parser, no renderer.
- **`nowui-macros`** — `#[derive(NowUiState)]`, a proc-macro that generates `get`/`set`/`call`
  reflection glue for a plain Rust struct. Re-exported through `nowui-core` so consumers only
  ever add one dependency.
- **`nowui-render`** — the tiny-skia `SkiaPainter` implementation of the `Painter` trait, plus
  the softbuffer presentation bridge.
- **`nowui-runtime`** — the `#` import loader, the semantic pass (AST → arena, including dynamic
  `if`/`for` region expansion), the expression evaluator (`dynamic.rs`), the transition driver,
  and the winit `App<S: NowUiState>` (lib + a thin CLI binary `nowui`) that ties state,
  layout, and paint together every redraw.

### The one hard architectural rule

**`nowui-core` must never import `chumsky` or `tiny-skia`.** The model stays testable in
isolation and the renderer stays swappable. If you need syntax or render types in core, you're
putting something in the wrong crate. Dependency arrows point one direction only:
`nowui-syntax` / `nowui-render` → (never) `nowui-core` → (never) `nowui-runtime`.

### Architecture decisions (keep consistent with these)

- **Node arena, not a recursive owned tree:** flat `Vec<Node>` + `NodeId(u32)` indices, with
  **no parent pointers**. Deliberate — avoids borrow-checker fights, makes focus/hover references
  cheap. A node that needs its ancestor (e.g. a `MenuItem` closing its own `Menu`) can't walk up;
  the caller that already knows both ids (`App`, which owns the whole arena) does the work
  instead. Do not refactor into `struct Node { children: Vec<Node> }`.
- **Layers** = `Vec<Layer>`, each its own layout root, composited back-to-front. Hit-testing goes
  front-to-back (topmost layer wins).
- **`Painter` trait is the render boundary** (`fill_rect`, `stroke_rect`, `draw_text`,
  `push_clip`/`pop_clip`, `measure_text`, `push_transform`/`push_opacity`). tiny-skia is one impl.
  "Retained" refers to the tree, not cached draw commands — the paint pass re-walks the tree each
  redraw; don't add draw-command caching until profiling demands it.
- **Solver** is a compact two-pass measure-then-distribute (a flex approximation: no min/max or
  wrap) plus CSS-grid-lite (`Display::Grid`: fixed/auto/fr tracks, row-major auto-place with
  span — no named lines/`minmax()`/`auto-fit`/dense packing). Swappable for `taffy` later
  without touching the arena or painter.
- **`Style::radius` is `Edges`, not `f32`** — four independent corner radii (CSS clockwise-from-
  top-left order): `top`=top-left, `right`=top-right, `bottom`=bottom-right, `left`=bottom-left.
- **softbuffer bridge:** tiny-skia's `Pixmap` is RGBA8 premultiplied; softbuffer wants `0RGB` u32.
  An opaque background is filled first (so premultiplied == straight), then packed
  `(r<<16)|(g<<8)|b`.

### Runtime gotchas (learned the hard way — don't regress these)

- **`request_redraw()` from inside `RedrawRequested` is not a reliable way to keep animating.**
  Driving continued redraws (e.g. an in-flight `transition`) must go through `ControlFlow`
  directly: `event_loop.set_control_flow(ControlFlow::Poll)` while `Transitions::any_active()`,
  back to `ControlFlow::Wait` once it isn't (`App::redraw` takes `&ActiveEventLoop` for exactly
  this). On Windows, a self-requested `request_redraw()`-only scheme visibly stalls — the redraw
  gets coalesced with the current frame instead of scheduling a new one.
- **Diagnosing "the style value looks right but nothing on screen changed":** verify the
  *animated* (post-`Transitions::step`) value with a temporary `eprintln!`, not just the target,
  and check the actual redraw count — a suspiciously low count means frames aren't being pumped
  (see the `ControlFlow` gotcha above) before suspecting style-resolution logic.

### Solver gotchas

- **Pass 2 (`arrange`) must reuse pass 1 (`measure`)'s memoized sizes, never re-derive them.**
  `measure()` memoizes every node's `Size` into a `HashMap<NodeId, Size>` (`sizes` in `solve()`),
  threaded through `arrange()`. A from-scratch re-estimate in pass 2 (e.g. a flat placeholder
  size for anything that isn't `Text`/`Button`) silently collapses any Hug-sized container with
  real content to a wrong flat default — invisible with placeholder content, obvious with real
  text/nested widgets.

### Parser gotchas

1. **Comments:** whitespace skipping must also eat `//` line comments — use the `pad()` helper at
   structural boundaries, not bare `.padded()`.
2. **Style key** is `ident ('-' ident)*`, where `-` only joins when followed by a key char
   (lookahead) — otherwise `p-[..]` folds the `-` into the key. Build the key `String` with
   `.then(...).map(...)`; don't use chumsky `.chain()` (its two `Chain` impls make `T` ambiguous).
3. **Style value** takes an optional leading `-` then `[...]` — the dash between key and bracket
   is consumed on the value side.
4. **`{ }` ambiguity:** bindings `{key: value}` and child blocks `{ Widget... }` both open with
   `{`. `node()` parses them as two independent optional trailing slots —
   `bindings().or_not()` then `child_block.clone().or_not()`, **not** an either-or `choice` — so a
   widget can have bindings, children, both (`Menu`, e.g., needs `{onClick: ...}` on itself *and*
   a real `{ MenuItem ... }` child list), or neither. Each slot's own `.or_not()` disambiguates on
   *content*, not position: `bindings()` on an actual child block fails to match
   `ident ':' bind_value, ...` and un-consumes the `{` cleanly, letting `child_block.or_not()`
   retry the same `{`. Don't reintroduce a single either-or choice to "fix" a backtracking issue
   here — disambiguate on content instead.
5. **Bare-flag styles vs. the next sibling's `kind`:** a bare style flag (`grid`, `row`) and a
   widget `kind` are both plain identifiers with nothing syntactically between them but
   whitespace. A style key's first character must be lowercase or `_` (`key_start`), matching the
   convention that widget kinds are Capitalized — otherwise `style().repeated()` eats the next
   sibling's `kind` as one more bare flag and two sibling nodes silently merge into one.
6. **`key_char` includes `/` and `.`** (for Tailwind fraction/decimal-scale classes like `w-1/2`,
   `py-3.5`). Neither can be a key's *first* character (`key_start` still requires
   lowercase/`_`), so this doesn't reopen gotcha #5's ambiguity.

### Build & test discipline

- Fix and build crate-by-crate in dependency order: **syntax → core → render → runtime**. Errors
  in higher crates often clear once lower ones compile.
- `cargo test -p nowui-syntax` — parser tests. Fast, no window. Add a test for every grammar
  change, in the same commit.
- `cargo test -p nowui-core` — solver/paint tests on hand-built arenas. No display needed. Add a
  hand-built-arena assertion for every solver change.
- `cargo test -p nowui-runtime` — semantic pass, reactivity, dynamic regions, app interaction
  logic (click/drag/keyboard handling), all against synthetic `Ui`s — no real window.
- `cargo test --workspace` before calling a change done.

---

## Project/Workspace layout

```text
nowui-syntax/    chumsky parser -> AST        (no core, no render deps); also the `if`/`for`
                 control-flow grammar and its `Expr` sub-language (dotted paths, comparisons,
                 &&/||/!)
nowui-core/      arena, Style, tailwind tokens, geometry, solver, paint walk, Painter trait,
                 NowUiState trait / StateValue (incl. `List`/`Object`) / Event (reactivity
                 interface), text_input.rs (cursor/selection/IME string math)
nowui-macros/    #[derive(NowUiState)] proc-macro (reflection glue), re-exported by nowui-core
nowui-render/    tiny-skia SkiaPainter + softbuffer bridge
nowui-runtime/   loader (# imports), semantic pass (incl. dynamic if/for region expansion),
                 dynamic.rs (expression evaluator + loop-variable substitution), transitions
                 driver, winit app (lib + binary `nowui`); generic App<S: NowUiState> resolves
                 values, dispatches events, and refreshes dynamic regions every redraw
examples/counter-app/           standalone workspace member (own Cargo.toml, package
                                 `nowui-login-app`, binary `login-app`) — a login-form-shaped
                                 reactivity demo exercising `if`/`else if`/`else`, `for`, and a
                                 nested `Vec<Row>` state field. `src/login.nowui` is bundled
                                 straight into the `login-app` binary via `#[nowui(view(
                                 "/login.nowui"))]` on `src/main.rs`'s `App` — no `.nowui` file
                                 needed on disk at runtime. `src/demo.nowui` is the same kind of
                                 full-feature showcase (imports, flex + grid, color/spacing/
                                 typography scales, borders/radius, opacity, 2D transforms,
                                 hover/active transitions, position-relative/absolute, z-index,
                                 Checkbox, Dropdown, Menu, Slider, ProgressBar, scroll-v), loadable
                                 from disk via `run_path` for iterating on it without a rebuild.
                                 `cargo run -p nowui-login-app`.
nowui-runtime/examples/counter.rs + counter.nowui   a smaller `#[derive(NowUiState)]`
                                 end-to-end example (increment/decrement counter), loaded from
                                 disk via `run_path` — `cargo run -p nowui-runtime --example
                                 counter`.
```

### Workspace members (`Cargo.toml`)

```toml
[workspace]
members = [
    "nowui-syntax", "nowui-core", "nowui-macros",
    "nowui-render", "nowui-runtime", "examples/counter-app",
]
```

### Running things

```sh
cargo test -p nowui-syntax                                    # parser, no window
cargo test -p nowui-core                                      # solver/paint, no window
cargo test -p nowui-runtime                                   # semantic/reactivity/app, no window
cargo test --workspace                                        # everything

cargo run -p nowui-runtime -- examples/counter-app/src/login.nowui App   # opens a window, no Rust state
cargo run -p nowui-login-app                                             # opens a window, bundled .nowui + real state
cargo run -p nowui-runtime --example counter                             # opens a window, on-disk .nowui + real state
```

### Roadmap status (each step runnable before the next)

1. ✅ Parser green — `cargo test -p nowui-syntax` passes.
2. ✅ Solver green on hand-built arenas — `cargo test -p nowui-core` passes.
3. ✅ Boxes on screen — the reference login layout renders correctly.
4. ✅ Real text (cosmic-text) — `draw_text`/`measure_text` shape and rasterize actual glyphs.
5. ✅ Input + focus — `Checkbox`/`Dropdown`/`Menu` toggle, all `EVENT_BINDING_KEYS` dispatch,
   `TextInput` has real cursor/selection/IME.
6. ✅ Reactivity — `NowUiState` + `#[derive(NowUiState)]` + generic `App<S>`; `{value: ...}`
   bindings, `${state.path}` backtick interpolation, `${state.path}` style-bracket interpolation,
   and `if`/`for` dynamic regions all resolve every redraw against live state.
7. ⬜ Per-layer pixmap caching — re-rasterize only dirty layers, then composite.

---

## `.nowui` syntax, with examples

Colon-delimited, brace-nested. NOT whitespace-sensitive. `//` line comments allowed.

### File shape and imports

```nowui
# widgets/button_row.nowui   // whole-file import: only valid at top level, between layout: defs

layout: Login(state) w-[fill] h-[fill] {
  // ...
}

layout: App w-[fill] h-[fill] {
  Login state=state
}
```

`#` imports are resolved relative to the *importing* file's own directory, inlined in place, and
deduped/cycle-broken via a canonical-path `visited` set — diamond imports (two files importing
the same third file) and import cycles are both handled for free.

### Widget grammar (fixed argument order)

```text
Kind  arg=value...  `string`...  style-[value]...  { bindings }  { children }
```

Both trailing `{ }` blocks are optional and independent — a widget can have bindings only,
children only, both, or neither:

```nowui
Menu `Preferences` w-[400px] {onClick: state.onMenuClick} {
  MenuItem `Open Preferences` {onClick: state.onOpenPrefs}
}
```

### `layout:` definitions — reusable, parameterized widgets

```nowui
layout: Card(title, subtitle) bg-white rounded-lg p-6 {
  Text `${title}` font-semibold text-lg
  Text `${subtitle}` text-gray-500 text-sm
}

layout: App w-[fill] h-[fill] {
  Card title="Welcome" subtitle="Sign in to continue"
}
```

`Name(params) { ... }` defines it; `Name arg=value` uses it. Custom widgets and layouts are the
same mechanism, expanded before layout solving. Args are named. Expansion is guarded against
recursive definitions with a depth cap.

### Backtick strings — text content, with optional interpolation

```nowui
Text `Plain text, no interpolation`
Text `Count: ${state.counter.count}!`          // literal text and ${...} freely mixed
TextInput `` `Enter Username`                  // first backtick = current value, second = placeholder
```

`${var}` or a dotted state path (`${state.counter.count}`) is resolved at **runtime**, re-rendered
every redraw by `App::resolve_templates` against live state — not baked in at parse time. An
all-literal node's `templates` stays empty (no extra per-frame cost). An empty `` `` `` backtick
is still meaningful — it holds a positional slot (e.g. `TextInput`'s label vs. placeholder).

### Styles

Generic `key-[value]` tokens, bare flags (`grid`), or compact Tailwind-scale classes (`p-4`,
`bg-blue-500`, `grid-cols-3`) — parsed identically as "a key, optionally with a bracket value."

```nowui
Container w-[fill] h-[hug] p-4 gap-2 bg-gray-100 rounded-lg
Text text-lg font-semibold text-blue-600
Button hover:bg-blue-700 active:scale-95 sm:w-[440px] transition duration-150
```

- `variant:` prefix (`hover:`, `focus:`, `active:`, `sm:`/`md:`/`lg:`/`xl:`/`2xl:`) folds into the
  key string at parse time, split back out in the semantic pass. Only a single prefix is
  supported — no stacked variants (`sm:hover:x`).
- A bracket value can itself be a `${state.path}` interpolation, but only when the *whole*
  bracket is the interpolation — `w-[${state.myWidth}]` works, `"10${x}px"` does not.
- Sizing primitives that are NowUI's own (not Tailwind): `w-[fill]`, `w-[fill-2]` (flex weight
  2), `w-[hug]`, `w-[440px]`. Tailwind's own `w-4`, `w-1/2`, `w-full` resolve to
  `Sizing::Fixed`/`Sizing::Percent` instead.

#### Tailwind v4 vocabulary supported

Spacing/sizing (`p-*`/`m-*`/`gap-*`/`w-*`/`h-*`, fractions like `w-1/2`, `w-full`), the full
22-family × 11-shade color palette (`bg-*`/`text-*`/`border-*`), typography (`text-{size}`,
`font-{weight}`, `leading-*`, `tracking-*`), flexbox (`row`/`col`/`row-reverse`/`col-reverse`,
`items-*`, `justify-*`), CSS grid (`grid`, `grid-cols-*`, `grid-rows-*`, `col-span-*`,
`row-span-*`), borders + per-corner radius (`rounded-*`), `opacity-*`, 2D transforms
(`translate-x/y-*`, `scale*`, `rotate-*`, `skew-x/y-*`), transitions (`transition`, `duration-*`,
`ease-*`, `delay-*`), positioning (`position-static`/`position-relative`/`position-absolute`,
`left-*`/`right-*`/`top-*`/`bottom-*`), scrolling (`scroll-h`/`scroll-v`), and
`hover:`/`focus:`/`active:` plus responsive variants.

#### Explicitly out of scope

Don't half-implement these — either build them properly with the state/rendering model they need,
or leave them as unknown-key warnings:

- `dark:`, `group-*`/`peer-*` — no theme or group/peer-state model exists to drive them.
- Stacked variants (`sm:hover:x`).
- 3D transforms, filters/backdrop-filters, box-shadow, `@keyframes` — the renderer is a 2D CPU
  rasterizer with no shadow/blur pipeline and only single-property `transition` interpolation.
- CSS Grid beyond fixed/auto/fr tracks + row-major auto-placement with span (no `minmax()`,
  `auto-fit`/`auto-fill`, named lines, dense packing).
- A `display: grid` container has no intrinsic Hug size of its own (its `fr` tracks only claim
  space once the container already has a definite size, same as real CSS) — give it an explicit
  `w-full`/`w-[…]`.

### Bindings: `{value: ...}` and events

```nowui
Checkbox `Enable notifications` {value: state.notificationsEnabled}
Button `SAVE` {onClick: state.save}
TextInput `` `Username` {value: state.username}
Slider {value: state.volume}
```

Any widget can carry a `{value: state.path}` binding (read by `Text`/`Checkbox`/`Dropdown`/
`Slider`/`ProgressBar`/`TextInput`) plus any of the event keys: `onClick`, `onMouseMove`,
`onMouseDown`, `onMouseUp`, `onKeyPress`, `onKeyDown`, `onKeyUp`, `onResize`. Bindings are rooted
at the literal `state` segment (`state.counter.increment`) — stripped before crossing into the
Rust-side `NowUiState` reflection boundary.

### `if`/`else if`/`else` and `for` — dynamic regions

Brace-delimited (reuses the same child-block parser every widget uses), re-expanded live against
state on every redraw — this changes which nodes *exist*, not just a value:

```nowui
if state.username.length > 3 && state.username.length < 8 {
  Text `Password` text-gray-700 text-sm
  TextInput `` `Enter Password` {value: state.password, mask: true}
} else if state.username.length >= 8 {
  Text `Username is too long` text-red-600 text-sm
} else {
  Text `Enter your username first` text-gray-500 text-sm
}

Grid grid grid-cols-2 gap-4 w-full {
  for row in state.rows {
    Checkbox `Remember me`
    Text `${row.label}` text-right
  }
}
```

- `Expr` is deliberately non-Turing-complete: literals (`true`/`false`/numbers/`"quoted
  strings"`), dotted paths, unary `!`, comparisons (`==`/`!=`/`<`/`<=`/`>`/`>=`, not chained),
  `&&`/`||`, parenthesized grouping. No arithmetic. Expression string literals use `"..."`
  (backticks stay reserved for widget text templates).
- `.length` is a pseudo-property (chars for a `Str`, item count for a `List`) — tried as a real
  field path first, so something genuinely named `length` still resolves correctly.
- `for x in state.rows` makes `${x}` (or `${x.field}` for a list of struct-typed items) usable
  inside backtick templates in the loop body — not inside a style bracket, and not inside a
  nested `if`/`for` condition in the same body.
- A `for`'s generated children splice in as **flat siblings**, not wrapped in an extra container
  — critical for e.g. a `for` inside `Grid grid-cols-2`, where each iteration's nodes must become
  the grid's own cells.
- Unrelated redraws (a hover, a transition tick) leave an unchanged region's node ids untouched —
  a `TextInput` inside one doesn't lose focus/cursor state for no reason.
- Known limitation: no node-removal/GC. Rebuilding a region doesn't free its old arena nodes —
  harmless (never painted/hit-tested again) but wasteful for a frequently-changing `for` list.

### Widgets

**`Text`** — `` Text `content` styles... ``. Read-only; can carry a `{value: state.path}` binding
too (`display_string` renders any `StateValue`).

**`Button`** — `` Button `Label` styles... {onClick: state.handler} ``.

**`Checkbox`** — `` Checkbox `Label` styles... {value: state.checked} ``. Toggles on click.
Styleable: `bg` fills the box, `border-color` (falls back to `text-color`) strokes it,
`rounded-*`/`radius` rounds box and checked-mark, `text-color` is the mark + label color.

**`TextInput`** — real cursor/selection/IME, click-to-position, drag-to-select, horizontal
scroll-follow-caret:

```nowui
TextInput `` `Enter Username` w-full bg-gray-100 rounded p-[10px] {value: state.username}
TextInput `` `Password` {value: state.password, mask: true}
TextInput `` `Notes` multi h-[120px] {value: state.notes}     // multiline: wraps + scrolls vertically
```

First backtick = current value (`label`, not append-only — it's the live bound value), second =
placeholder (shown only while the value is empty). `mask: true` shows bullets. `multi` (bare
flag) switches to word-wrapped, vertically-scrolling multi-line editing; caret/selection are a
hard-line model (splits on `\n` only — a hard line that itself word-wraps still renders/edits
correctly, but the overlay doesn't track the extra wrapped visual lines).

**`Dropdown`** — first backtick is the placeholder, every backtick after it is an option:

```nowui
Dropdown `Choose a theme` `Light` `Dark` `System` w-full border rounded {value: state.theme}
```

The open option list **floats over the page** — it doesn't push later siblings down, isn't
clipped by its container, and isn't reachable through normal hit-testing (dedicated popup-rect
hit-test in the runtime). Styleable: `border-color`/`rounded`/`radius` on the box, `bg`/
`text-color` on both box and popup panel.

**`Menu`/`MenuItem`** — a clickable header whose child list is a **floating popup below the
header** (same principle as `Dropdown`'s popup), but with real arena-node children instead of
flattened strings, so each `MenuItem` can have its own independent styles/`onClick`/further
children:

```nowui
Menu `Preferences` w-[400px] bg-white border rounded-lg {onClick: state.onMenuClick} {
  MenuItem `Open Preferences` p-3 hover:bg-gray-100 {onClick: state.onOpenPrefs}
  MenuItem `Sign Out` p-3 hover:bg-gray-100 text-red-600 {onClick: state.onSignOut}
}
```

A `Menu` with no children never produces a popup, open or not. Clicking a `MenuItem` dispatches
its *own* `onClick` (independent of the `Menu`'s own `onClick`) and closes the popup; clicking
elsewhere closes every other open `Menu`/`Dropdown`. One-way bound (`onClick` only) — unlike
`Dropdown`, there's no single "selected value" to write back.

**`Slider`** — a draggable `0.0..=1.0` value:

```nowui
Slider w-full text-blue-600 border-gray-200 {value: 60}
Slider w-full text-blue-600 {value: state.volume}
```

`{value: N}` as a literal 0..=100 number sets the starting position; a `state.*` path binds it
live. `text-color` is the track-fill/thumb color, `border-color` is the empty-track color.

**`ProgressBar`** — same styling/geometry convention as `Slider`, read-only (no drag):

```nowui
ProgressBar w-full text-emerald-500 border-gray-200 {value: 82}
```

**`scroll-h`/`scroll-v`** — clips overflow along that axis, mouse wheel pans it:

```nowui
Container scroll-v h-[160px] w-full border rounded gap-1 p-2 {
  Text `Row one`
  Text `Row two`
}
```

Thumb/track reuse `border-color` (falls back to neutral gray) at full/low alpha — no dedicated
`scrollbar-*` class family.

**`position-absolute`/`position-relative`** — containing block is always the *direct* parent's
content box (one level only, not the nearest positioned ancestor like real CSS):

```nowui
Container position-relative w-[hug] h-[hug] {
  Text `Alerts`
  Container position-absolute top-[-8px] right-[-14px] bg-red-500 rounded-full px-[7px] {
    Text `3` text-white
  }
}
```

An `Absolute` child escapes its direct parent's own paint clip too (so a badge pinned outside its
box via a negative offset isn't cut off), while still respecting any *further* ancestor clip.

**`z-index-[N]`/`z-index-N`** — reorders paint order only, among sibling nodes (never layout or
hit-testing), stable-sorted so equal-index ties keep source order:

```nowui
Container position-relative w-[960px] h-[160px] {
  Card position-absolute top-[20px] left-[0px] z-index-20 { Text `Front — painted last` }
  Card position-absolute top-[0px] left-[220px] z-index-1 { Text `Back — painted first` }
}
```

---

## Rust sample app

Three ways to get from a `.nowui` file to a running window, depending on where the source lives:

- **`nowui_runtime::run(entry, state)`** — `.nowui` source **bundled into the binary** via
  `#[nowui(view("/path.nowui"))]`. Use when shipping a real app: no `.nowui` file needed on disk
  at runtime.
- **`nowui_runtime::run_path(path, entry, state)`** — `.nowui` source loaded from disk at
  runtime. Use when iterating on a `.nowui` file without a rebuild, or one with `#` imports.
- **the `nowui` CLI binary** (`nowui-runtime/src/main.rs`) — loaded from disk, `NoState`. Use for
  quickly previewing an arbitrary `.nowui` file with no Rust state at all.

### Bundling a `.nowui` file into the executable — `#[nowui(view("/path.nowui"))]`

Add the attribute alongside `#[derive(NowUiState)]` on your top-level state struct. The path is
resolved **relative to that crate's own `src/` directory** and embedded at compile time via
`include_str!` — the string literally becomes part of the binary, so nothing needs to exist on
disk at runtime. Then call `nowui_runtime::run(entry, state)` with no path argument at all:

```rust
use std::process::ExitCode;
use nowui_core::NowUiState;

// "/login.nowui" resolves to this crate's `src/login.nowui` — embedded via
// `include_str!` at compile time by the derive macro.
#[derive(Default, Clone, NowUiState)]
#[nowui(view("/login.nowui"))]
pub struct App {
    username: String,
    password: String,
    rows: Vec<Row>,
}

#[derive(Default, Clone, NowUiState)]
pub struct Row {
    id: String,
    label: String,
}

fn main() -> ExitCode {
    nowui_runtime::run("App", App {
        username: String::new(),
        password: String::new(),
        rows: vec![Row { id: "x".to_string(), label: "x".to_string() }],
    })
}
```

This is the real shape of `examples/counter-app/src/main.rs` (package `nowui-login-app`, binary
`login-app`; `login.nowui` lives at `examples/counter-app/src/login.nowui`). `rows: Vec<Row>`
(where `Row` itself derives `NowUiState`) resolves to
`StateValue::List(Vec<StateValue::Object(...)>)` for `login.nowui`'s `for row in state.rows`
loop — each `Object` snapshots `Row`'s fields, letting the loop body use `${row.label}`. Run it:
`cargo run -p nowui-login-app`.

Mechanics: `NowUiState` has three methods for this, all defaulting to `None` and all `where Self:
Sized` (keeps the trait object-safe for the `&dyn NowUiState` uses elsewhere, since a
receiverless associated function can't go through a vtable):

- `nowui_view() -> Option<&'static str>` — the entry file's own embedded source.
- `nowui_view_path() -> Option<&'static str>` — the literal string given to `view(...)` (e.g.
  `"/login.nowui"`), so `run` can work out the entry's own `#`-import base directory.
- `nowui_view_imports() -> Option<&'static [(&'static str, &'static str)]>` — every file the
  entry transitively `#`-imports, also embedded, as `(key, source)` pairs.

The derive overrides all three together whenever `#[nowui(view(...))]` is present. At
macro-expansion time (`nowui-macros`'s `build_embedded_view`), it reads the entry file, **parses
it** (`nowui-macros` depends on `nowui-syntax` for exactly this — not a violation of
`nowui-core`'s "no chumsky" hard rule, which is about the runtime *model* crate staying
parser-agnostic; this proc-macro runs entirely at the consuming crate's compile time and ships in
no binary), finds its `#`-import directives, and recurses into each imported file the same
way — reading, parsing, collecting its own imports — depth-first, deduping diamond imports and
breaking cycles via a `visited` set keyed by `nowui_syntax::join_import_path`'s normalized,
`/`-separated path (purely lexical — no `Path::canonicalize`, since these files won't exist on
disk anymore once resolved at runtime; consistent as long as both the macro and the runtime
loader compute keys with the exact same function, which they do, from the one shared
`nowui-syntax` crate both already depend on). Every file's content is embedded via
`include_str!` on its own absolute path (not spliced from the string the macro read) so rustc
gets real compile-time dependency tracking — the crate rebuilds if any embedded `.nowui` file
changes, not just the entry.

At runtime, `run` calls `S::nowui_view()`/`nowui_view_path()`/`nowui_view_imports()` and feeds
them to `loader::load_and_resolve_bundled(entry_source, entry_dir, imports)` — the bundled
equivalent of `load_and_resolve`, resolving each `#` import it encounters by recomputing the same
`join_import_path` key and looking it up in the embedded map, instead of reading a file. No
filesystem access at all. `run` fails with a clear error (not a panic) if `nowui_view()` is
`None`, pointing you at `run_path` instead.

### Loading a `.nowui` file from disk at runtime — `nowui_runtime::run_path`

No `#[nowui(view(...))]` needed; give the path directly, same as the pre-bundling API. This still
resolves `#` imports (via `loader::load_and_resolve`), so it's the right choice for a file that
imports others, or one you want to edit and re-run without recompiling:

```rust
use std::process::ExitCode;
use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
struct AppState {
    counter: Counter,
}

// Callable methods aren't auto-discovered from `impl Counter` — a derive
// macro can't see a separate impl block — so list them explicitly.
#[derive(Default, Clone, NowUiState)]
#[nowui(methods(increment, decrement))]
struct Counter {
    count: i64,
}

impl Counter {
    fn increment(&mut self, _event: &Event) { self.count += 1; }
    fn decrement(&mut self, _event: &Event) { self.count -= 1; }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/counter.nowui");
    nowui_runtime::run_path(nowui_file, "App", AppState::default())
}
```

This is `nowui-runtime/examples/counter.rs` + `nowui-runtime/examples/counter.nowui` —
`cargo run -p nowui-runtime --example counter`. The matching `.nowui` bindings: `{value:
state.counter.count}` (a `Text` template), `{onClick: state.counter.increment}` (a `Button`).
`NowUiState::get`/`set`/`call` walk the dotted path one named field at a time; a field whose type
isn't a recognized scalar is assumed to itself derive `NowUiState` and gets a delegating arm
(`counter: Counter` → `Counter` also derives it).

### The `NowUiState` contract (`nowui-core/src/state.rs`)

```rust
pub trait NowUiState {
    fn get(&self, path: &[&str]) -> Option<StateValue>;
    fn set(&mut self, path: &[&str], value: StateValue) -> bool;
    fn call(&mut self, path: &[&str], event: &mut Event) -> bool;
    fn to_state_value(&self) -> StateValue { StateValue::Object(vec![]) } // for Vec<T> fields

    // For #[nowui(view("/path.nowui"))] — see "Bundling a .nowui file..." above.
    fn nowui_view() -> Option<&'static str> where Self: Sized { None }
    fn nowui_view_path() -> Option<&'static str> where Self: Sized { None }
    fn nowui_view_imports() -> Option<&'static [(&'static str, &'static str)]> where Self: Sized { None }
}
```

`#[derive(NowUiState)]` generates this for a named-field struct: `String` → `StateValue::Str`,
`bool` → `Bool`, any integer → `Int(i64)`, `f32`/`f64` → `Float(f64)` (kept separate from `Int`,
not collapsed into one `Number`, so display code never has to guess int-vs-float back from a
value). `Event` carries `pub node: &'a mut Node` — a handler can mutate the originating widget's
`style`/`kind` directly (`event.node.style.opacity = 0.5`), which is why `call` takes `&mut
Event`, not `&Event`.

For a no-Rust-state file, use the CLI binary directly — `nowui_core::NoState` is a no-op impl
where every `get`/`set`/`call` returns `None`/`false`:

```sh
cargo run -p nowui-runtime -- path/to/file.nowui EntryLayoutName
```
