# CLAUDE.md

Standing instructions for working in the NowUI repository. Read this before making changes.

---

## Project Overview

NowUI is a file-based, retained-mode UI toolkit for Rust with a custom Tailwind-flavored syntax.
UIs are described in `.nowui` files, parsed to an AST, expanded into a flat node arena, laid out
with a two-pass layout solver, and rasterized to a window through the `Painter` trait — either
CPU (`tiny-skia`, via `nowui-render`) or GPU (`vello`/`wgpu`, via `nowui-render-gpu`; the default —
see `nowui_runtime::Backend`). The reference target used throughout development is a login
screen: dark top bar, blue field, centered white card with username/password inputs and a SIGN IN
button (`examples/counter-app/src/login.nowui`).

### Build & test discipline

- Fix and build crate-by-crate in dependency order: **syntax → core → text → render/render-gpu →
  runtime**. Errors in higher crates often clear once lower ones compile.
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
nowui-text/      shared cosmic-text shaping/measurement (TextContext, shape_text, measure) — used
                 by both Painter backends below, so shaping logic exists exactly once
nowui-render/    tiny-skia SkiaPainter (CPU Painter backend) + softbuffer presentation bridge
nowui-render-gpu/ vello/wgpu GpuPainter (GPU Painter backend, the default — see
                 nowui_runtime::Backend) + GpuSurfaceState (wgpu surface/device/Renderer owned
                 across the window's lifetime)
nowui-runtime/   loader (# imports), semantic pass (incl. dynamic if/for region expansion),
                 dynamic.rs (expression evaluator + loop-variable substitution), transitions
                 driver, winit app (lib + binary `nowui`); generic App<S: NowUiState> resolves
                 values, dispatches events, and refreshes dynamic regions every redraw
nowui-lsp/       standalone editor-tooling crate — a language server (binary `nowui-lsp`, over
                 stdio) for `.nowui` files: syntax highlighting via `textDocument/
                 semanticTokens/full` (tokenizer.rs — a lexer independent of nowui-syntax's AST,
                 since ast::Node carries no source spans) and parse-error diagnostics via
                 nowui_syntax::parse. Depends on nowui-syntax only (compile-time editor tooling,
                 not shipped in any NowUI app binary — doesn't participate in the "no chumsky in
                 nowui-core" hard rule, which is about the runtime model crate specifically).
nowui-extension/ the VS Code client (TypeScript/npm, not a Cargo workspace member) — registers
                 the `nowui` language and spawns nowui-lsp as a language client. See its own
                 README.md for the dev workflow (npm install && npm run compile, then F5).
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
nowui-runtime/examples/datetime_demo.rs + datetime_demo.nowui   a showcase of `Date`/`Time`/
                                 `DateTime` (staged Confirm/Cancel popups, the draggable analog
                                 clock, `minYear`/`maxYear`-bounded year dropdown, the DateTime
                                 Calendar/Clock tab toggle), loaded from disk via `run_path` —
                                 `cargo run -p nowui-runtime --example datetime_demo`.
```

### Workspace members (`Cargo.toml`)

```toml
[workspace]
members = [
    "nowui-syntax", "nowui-core", "nowui-macros", "nowui-text",
    "nowui-render", "nowui-render-gpu", "nowui-runtime", "nowui-lsp", "examples/counter-app",
]
```

`nowui-extension` is a separate npm project (not a Cargo workspace member — it has no Rust code
of its own) living alongside these at the repo root.

### Running things

```sh
cargo test -p nowui-syntax                                    # parser, no window
cargo test -p nowui-core                                      # solver/paint, no window
cargo test -p nowui-runtime                                   # semantic/reactivity/app, no window
cargo test -p nowui-lsp                                        # tokenizer/line-index, no editor needed
cargo test -p nowui-render-gpu --test offscreen                # GpuPainter vs SkiaPainter, headless GPU, no window
cargo test --workspace                                        # everything

# All three open a real window via nowui_runtime::Backend::Gpu (the default — vello/wgpu) unless
# the binary explicitly calls run_with_backend(..., Backend::Cpu) instead.
cargo run -p nowui-runtime -- examples/counter-app/src/login.nowui App   # opens a window, no Rust state
cargo run -p nowui-login-app                                             # opens a window, bundled .nowui + real state
cargo run -p nowui-runtime --example counter                             # opens a window, on-disk .nowui + real state
cargo run -p nowui-runtime --example datetime_demo                       # opens a window, Date/Time/DateTime showcase

cargo build -p nowui-lsp                                       # builds target/debug/nowui-lsp[.exe]
cd nowui-extension && npm install && npm run compile            # then F5 in VS Code — see its README.md
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
Button `${state.isSaving == true ? "Saving..." : "Save"}`   // ternary — see below
```

`${var}` or a dotted state path (`${state.counter.count}`) is resolved at **runtime**, re-rendered
every redraw by `App::resolve_templates` against live state — not baked in at parse time. An
all-literal node's `templates` stays empty (no extra per-frame cost). An empty `` `` `` backtick
is still meaningful — it holds a positional slot (e.g. `TextInput`'s label vs. placeholder).

`${...}` can also hold a full ternary — `cond ? then : else`, where `cond` is the same `Expr`
grammar `if`/`for` conditions already use (literals, dotted paths, comparisons, `&&`/`||`/`!`,
`.length`), and `then`/`else` are themselves `Expr` (usually `"quoted strings"`, but a nested
dotted path or ternary also works). `${state.foo}` alone still lowers to the same simple
`TemplatePart::Var` shape it always has — the ternary machinery only engages once a `?` shows up.
Implementation note: `nowui-core` can't hold a raw `Expr` (its own hard "no `nowui-syntax`
dependency" rule), so a ternary-bearing backtick is kept in its original, un-lowered form in
`nowui-runtime`'s own `Semantic::template_exprs` side table (keyed by `NodeId`) instead of being
lowered into `nowui_core::TemplatePart` — evaluated fresh every redraw via `dynamic::eval_expr`,
the exact same evaluator `if`/`for` conditions already use. A node with *any* ternary-bearing
backtick renders *all* of its own backticks through this side table (not a mixed per-argument
strategy) — `nowui_core::Node::templates` is left with harmless empty-`Lit` placeholders at those
indices. A `Variable`-aliased or `for`-loop-scoped name inside a ternary is resolved against its
scope once, at build time (`resolve_scoped_expr`) — same substitution a plain `Var` template part
already gets.

### Styles

Generic `key-[value]` tokens, bare flags (`grid`), or compact Tailwind-scale classes (`p-4`,
`bg-blue-500`, `grid-cols-3`) — parsed identically as "a key, optionally with a bracket value."

```nowui
Container w-[fill] h-[hug] p-4 gap-2 bg-gray-100 rounded-lg
Text text-lg font-semibold text-blue-600
Button hover:bg-blue-700 active:scale-95 sm:w-[440px] transition duration-150
```

- `variant:` prefix (`hover:`, `focus:`, `active:`, `disabled:`, `sm:`/`md:`/`lg:`/`xl:`/`2xl:`)
  folds into the key string at parse time, split back out in the semantic pass. Only a single
  prefix is supported — no stacked variants (`sm:hover:x`).
- **`disabled:`** — any widget can carry a `{disabled: state.path}` binding (a live `bool`,
  resolved every redraw into `Node::disabled`, same read-half-of-reactivity shape as `value`) plus
  `disabled:`-prefixed styles applied while it's `true` — applied *after* (so overriding)
  `hover:`/`focus:`/`active:` for whatever fields it touches:

  ```nowui
  Button `Save` disabled:text-[#FF0000] disabled:bg-[#FFFF00] {disabled: state.someDisabledBool}
  ```

  A disabled node's own bound events don't fire at all (`onClick`, `onMouseDown`, etc. — but not
  `onLoad`, which isn't real user interaction), and its own state-toggling interaction
  (`Checkbox`'s check, `Dropdown`/`Menu`/`Date`/`Time`/`DateTime`'s open-on-click, ...) doesn't
  happen either — same as a real HTML `disabled` attribute. `text` is accepted as a short bracket
  alias for `text-color` (matching `bg`'s own existing short-bracket-alias-for-`bg-color`
  precedent), so `disabled:text-[#FF0000]` and `disabled:text-color-[#FF0000]` are equivalent.
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
`Slider`/`ProgressBar`/`TextInput`/`Date`/`Time`/`DateTime`) plus any of the event keys: `onClick`,
`onMouseMove`, `onMouseDown`, `onMouseUp`, `onKeyPress`, `onKeyDown`, `onKeyUp`, `onResize`,
`onSelect` (`Date`/`Time`/`DateTime` only — fires when their popup's Confirm button commits a new
value). `Date`/`DateTime` also accept `{minYear: state.path}`/`{maxYear: state.path}`, bounding
their year dropdown (see the widget section below). Bindings are rooted at the literal `state`
segment (`state.counter.increment`) — stripped before crossing into the Rust-side `NowUiState`
reflection boundary.

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
- A `{key: x.field}` **binding** rooted at the loop variable (e.g. `{onClick: x.handleMe}`) is
  also rewritten per iteration — `dynamic::substitute_loop_var` replaces `x` with `state.rows.<N>`
  (the `for`'s own iterable path plus this item's index), so it dispatches through
  `nowui-macros`'s generated `call`/`get`/`set` as an indexed step into the `Vec<T>` field,
  landing on that one element's own method. Only works when the iterable is a plain dotted
  `state.*` path (not an expression) and `T` is itself a `NowUiState`, not a leaf scalar.
- A `for`'s generated children splice in as **flat siblings**, not wrapped in an extra container
  — critical for e.g. a `for` inside `Grid grid-cols-2`, where each iteration's nodes must become
  the grid's own cells.
- Unrelated redraws (a hover, a transition tick) leave an unchanged region's node ids untouched —
  a `TextInput` inside one doesn't lose focus/cursor state for no reason.
- A rebuilt region's old arena nodes are never painted/hit-tested again, but they aren't left to
  leak forever either — `Ui::gc()` (mark-and-sweep from every `Layer::root` plus `Ui::focus`) frees
  their heap payload (label strings, an `Image`'s decoded pixels, an abandoned subtree's own
  `Vec<NodeId>` children lists, ...). `nowui_runtime::App::redraw` and `nowui-designer`'s own
  `Chrome::refresh` both call it once per redraw, right after `Semantic::refresh_dynamic_regions`
  (the only point in a frame that can actually create new garbage). It deliberately does **not**
  shrink `Ui::nodes` or reuse a swept node's own `NodeId` for a later `push` — every `NodeId` an
  arena ever hands out stays permanently distinct, so holding one across a `gc()` call (a side
  table keyed by `NodeId`, a cached id in a test, ...) is always safe: a stale one just now points
  at an inert, empty `Container` tombstone instead of a dangling *or* — the real hazard a
  slot-reuse scheme would risk — a different, unrelated live node. The arena `Vec`'s own length
  still only grows, but that fixed per-slot overhead was never the actual cost this matters for.

### `Variable` — local scope aliases

`Variable name=value` declares a local alias, usable by every *later* sibling in the same body
(and their descendants) — a `let`, not a widget: it produces no arena node of its own.

```nowui
layout: T {
  Variable counters=state.counters
  if counters {
    Text `${counters.length}`
  }
}
```

- Parses with zero grammar changes — `Variable name=value` is just an ordinary widget use
  (`kind: "Variable"`, one `NamedArg`), the same shape a custom layout's own params already use.
  `Semantic::expand_children` special-cases the `"Variable"` kind and intercepts it before it ever
  reaches `expand()`/`primitive()` — it never becomes an arena node, so it also never emits an
  "unknown widget" warning.
- The value is resolved **eagerly** against the scope so far — reusing the exact `resolve_scoped_path`
  step a custom layout's own `bind_scope` already applies to its params — so `Variable b=a` (aliasing
  an earlier `Variable`, or a layout param, not `state` directly) chains correctly rather than
  capturing an unresolved bare name.
- Because the alias is stored as a path, not a resolved value, reads through it stay live — `if
  counters { ... }`/`${counters.field}` re-resolve `state.counters` fresh every redraw, the same as
  a direct `state.*` reference would.
- Works inside `for` bodies too: a loop's own `dynamic::substitute_loop_var` already rewrites a
  `NamedArg` rooted at the loop variable (`substitute_named_arg`) before `expand_children` ever
  sees `Variable x=row.value`, so the alias ends up pointing at that concrete iteration's own
  indexed path, not the bare loop-local name.
- `.length` (chars for a `Str`, item count for a `List`) resolves through a `Variable` alias inside
  an `if`/`for` condition (`dynamic::eval_expr`'s own path resolution handles it) — but **not**
  inside a plain `` `${...}` `` template, since `resolve::render_template` doesn't special-case
  `.length` at all. That's a pre-existing gap shared by every `.length` template reference, not
  something specific to `Variable`.

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

**`Dropdown`** — first backtick is the placeholder. Options are always real `DropdownItem`
children (there is no legacy plain-string-backtick form):

```nowui
Dropdown `Choose a person` w-full border rounded {
  onSelect: state.onSelectPerson, values: state.people
} {
  DropdownItem `` `-- choose a person --` default-selected
  DropdownItem `` `-- staff below --` disabled
  DropdownItem `alice` `Alice`
}
```

- **`DropdownItem`** (`` `id` `label` ``, plus the bare `default-selected`/`disabled` flags) is
  read directly out of the `Dropdown`'s own children at build time — it's data consumed into the
  `Dropdown`'s own `option_ids`/`options`/`option_disabled`/`static_items`, **never a real arena
  node of its own** (the same "data only, never becomes an independent arena node" precedent
  `Variable` already sets — not a real-child-tree widget like `Menu`/`MenuItem`). Static
  `DropdownItem`s always render *above* whatever a `values` binding contributes.
- **`disabled`**: a `DropdownItem` is disabled (greyed-out text, unselectable by click) either
  explicitly (the bare `disabled` flag) or implicitly whenever its own id is blank (`` `` `` — a
  common "-- choose one --" placeholder pattern). A disabled item can still be the *initial*
  selection via `default-selected` — same "a disabled placeholder `<option>` can still be the
  default" convention a real HTML `<select>`/React already has — only a *user click* on it is
  blocked (`Node::select_dropdown_by_id`/`select_dropdown_by_value`, being programmatic rather
  than a click, are **not** gated by `disabled`). A `values`-bound item can only be disabled via
  a blank id — the plain `DropdownItem` struct a `values` binding expects has no `disabled` field
  of its own to opt into.
- **`{values: state.path}`** binds a live `Vec<DropdownItem>` — a Rust struct with `label`/`id`
  string fields (`#[derive(NowUiState)] struct DropdownItem { label: String, id: String }`,
  matching how any other `Vec<T: NowUiState>` crosses into a `for` loop's iterable). Re-resolved
  every redraw (`nowui-runtime`'s `resolve_dropdown_values`) and appended after the static items.
  Selection is preserved *by id* across a rebuild — if the previously-selected id no longer
  exists in the new list, falls back to whichever static item declared `default-selected`, else
  clears to the placeholder.
- **`onSelect`** fires when an (enabled) option is picked (in addition to, not instead of, the
  ordinary `{value: state.path}` two-way write-back, which gets the option's own id).
  `Event::child_id`/`child_label` carry the just-selected item's own id/label — since a dropdown
  option isn't a real arena node `event.node` could otherwise point at (`event.node` is the
  `Dropdown` itself). `None` for every other event.
- **`Node::select_dropdown_by_id(id)`**/**`select_dropdown_by_value(label)`** let a handler
  change a `Dropdown`'s selection programmatically (not just read it, and not gated by
  `disabled`) — e.g. from the `Dropdown`'s own `onClick`/`onLoad`, where `event.node` is the
  `Dropdown` in question. Returns `false` (no-op) if this node isn't a `Dropdown` or no option
  matches.
- The open option list **floats over the page** — it doesn't push later siblings down, isn't
  reachable through normal hit-testing (dedicated popup-rect hit-test in the runtime), and never
  grows past `DROPDOWN_POPUP_MAX_H` (300px) tall — beyond that it clips and becomes vertically
  scrollable (mouse wheel, plus a thin scrollbar — same visual convention `scroll-v` containers
  already use, reusing `Node::scroll_offset`) instead of just growing to fit every option.
  Styleable: `border-color`/`rounded`/`radius` on the box, `bg`/`text-color` on both box and
  popup panel. The closed box's own caret is `FaChevronUp`/`FaChevronDown` (open/closed) from
  the embedded `nowui-icons` library — see the `Icon` widget
  section below for how that library is populated; a plain filled square if it isn't linked in
  (e.g. a bare `nowui-core` test `Ui`).

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

**`Date`/`Time`/`DateTime`** — a closed box holding `value` (or a placeholder while empty) plus a
floating picker popup, opened/closed by clicking the box like `Dropdown`/`Checkbox`. Styled
**exactly like `TextInput`** — no built-in box border/background of its own; `bg-*`/`border-*`/
`rounded-*`/`p-*`/`h-*` etc. are the *only* thing drawing the closed box (see `paint_picker_box`),
same as a plain `TextInput`. Its own icon glyph is `FaChevronUp`/`FaChevronDown` (open/closed),
same convention and fallback as `Dropdown`'s caret above:

```nowui
Date `Choose a date` w-full bg-white border rounded p-[10px] {
  value: state.birthday, minYear: state.minYear, maxYear: state.maxYear, onSelect: state.onBirthdayPicked
}
Time `Choose a time` with-seconds w-full bg-white border rounded p-[10px] {value: state.alarm, onSelect: state.onAlarmPicked}
DateTime `Choose both` w-full bg-white border rounded p-[10px] {
  value: state.meeting, minYear: state.minYear, maxYear: state.maxYear, onSelect: state.onMeetingPicked
}
```

- Value formats: `Date` is `DD/MM/YYYY`; `Time` is `HH:MM`, or `HH:MM:SS` with the `with-seconds`
  bare style flag; `DateTime` is the two joined by one space (`DD/MM/YYYY HH:MM[:SS]`). All date
  math/formatting/parsing/popup geometry lives in `nowui-core`'s `datetime` module — no external
  date/time crate (`datetime::now()` reads the system clock as **UTC**, not the OS's local
  timezone; no timezone database is linked in, matching the "don't half-implement it" convention
  below).
- **Staged vs. committed**: every popup edits a *staged* copy of the value (`NodeKind::Date`'s
  `picker: DatePickerState`, `Time`'s `picker: TimePickerState`, `DateTime`'s `date_picker`/
  `time_picker`) — clicking a day, dragging the clock hand, paging the year list, none of that
  touches `value` itself. **Confirm** commits the staged state into `value` and dispatches
  `onSelect`; **Cancel**, or clicking outside the popup, discards it and closes without saving.
  Every popup carries a Cancel/Confirm footer. Reopening the popup re-seeds the staged copy from
  `value` — or, if `value` is still empty, from the system clock's current date/time (so the
  calendar/clock always shows *something* concrete, without ever writing that default into
  `value` on its own).
- **`Date`'s popup**: a two-row header — a month row (`<`/`>` steps the month by one, wrapping the
  year at Dec/Jan) and a year row (`<`/`>` steps *only* the year, never the month; a `YYYY ▾`
  dropdown toggles a paged 12-year grid in place of the day grid, bounded by `minYear`/`maxYear` —
  default `now year ± 100` if unbound). Below that, weekday labels and a fixed 6x7 day grid; the
  staged day shows as a filled indigo circle.
- **`Time`'s popup**: a draggable analog dial. A header row of clickable hour/minute/[second
  with `with-seconds`] segments switches which ring the dial edits; dragging the hand (or clicking
  anywhere on the face) jumps the active ring's staged value to the angle under the cursor — the
  hour ring snaps to its 12 tick positions, the minute/second ring is continuous (any exact
  minute/second, not just 5-unit ticks). A two-way AM/PM toggle sits below the dial.
- **`DateTime`'s popup**: a two-button **CALENDAR**/**CLOCK** tab row switches which single
  sub-view is shown — never both at once. Picking a date on the Calendar tab or dragging the dial
  on the Clock tab only ever updates its own half of the staged state; one shared Cancel/Confirm
  footer commits (`datetime::join_datetime`) or discards *both* halves together, regardless of
  which tab was last active.
- **Popup placement**: every popup opens directly below its box, flipping above instead if it
  would overflow the bottom of the window, and clamped horizontally so it never runs off the
  left/right edge either (`datetime::place_popup`, driven by `Ui::viewport` — kept in sync by
  `layout::solve` every frame).
- **Page auto-scroll**: if a popup still doesn't fully fit even after `place_popup`'s own
  flip/clamp (e.g. the box sits somewhere with no fully-clear placement on either axis), the whole
  page pans via `Ui::auto_scroll` — the same sign convention as a `scroll-x`/`scroll-y`
  container's own `scroll_offset`, just applied to every layer's root in `layout::solve` instead of
  one container's children — so the popup ends up fully visible with 16px of breathing room past
  whichever edge(s) it overflowed (`datetime::reveal_scroll_delta`, `nowui-runtime`'s `App::
  update_auto_scroll`). This only ever fires once, on the rising edge of the popup opening — not
  every frame it stays open — so it doesn't fight a user's own `MouseWheel` scrolling away from an
  already-revealed popup; wheel input past that point pans further within `Ui::page_scroll_min`/
  `max` — the valid range, persisted **separately** from `auto_scroll`'s own current value the
  moment the popup is revealed, and left alone while it stays open. That separation matters:
  inferring the range from whether `auto_scroll` is currently non-zero collapses to nothing the
  instant the user scrolls back to exactly `0` (an ordinary position *within* the range, not the
  end of it) — which used to both hide the scrollbar and permanently disable scrolling back down
  again, a real regression this pair of fields exists to fix. Both reset to `(0, 0)` the moment no
  picker popup is open. While `page_scroll_min != page_scroll_max` on an axis, a thin translucent
  browser-style scrollbar (track + thumb, sized/positioned off that persisted range, not the
  current value) is drawn along the window's right/bottom edge (`paint::paint_page_scrollbars`) —
  visual only, not itself draggable.
  Since `auto_scroll` shifts every layer's root *origin* away from `(0, 0)`, the root's own
  background fill no longer covers the whole physical window on its own — `paint::paint` covers
  the entire `Ui::viewport` with the first layer's root's own `bg` color before painting anything
  else, so the strip of window `auto_scroll` reveals matches the app's actual background instead
  of showing raw clear-color.
- **Theme**: every popup's own internals (not the closed box, which follows the widget's own
  style) are a fixed white background / near-black text / indigo (Tailwind indigo-500/600/100)
  accent palette — hover shows a light-indigo highlight, a held mouse-button shows a darker one,
  computed live each redraw from `Ui::cursor`/`Ui::mouse_down` (these hand-drawn controls aren't
  real per-control arena nodes, so they can't carry their own `hover:`/`active:` style variants).

**`Image`** — a local file, `#`/relative-path-resolved local file, or a `http://`/`https://`
network URL, decoded via the `nowui-image` crate (png/gif/jpeg/bmp/webp — the `image` crate under
the hood, no `nowui-core` dependency, same "shared preprocessing, no renderer coupling" shape as
`nowui-text`):

```nowui
Image `assets/logo.png` w-[200px] h-[auto]                       // relative to this .nowui file
Image `../shared/banner.jpg` w-[auto] h-[150px]
Image `https://picsum.photos/seed/picsum/200/300` w-[64px] h-[64px]  // never bundled, always live
Image `assets/spinner.gif` w-[80px] h-[80px] loop                // loops playback; omit `loop`
                                                                   // to hold on the last frame
```

- **Relative local paths** are resolved by the *loader* (`nowui-runtime/src/loader.rs`'s
  `resolve_image_paths`), relative to the `.nowui` file that wrote them — the loader is the only
  pipeline stage that still has per-file directory context before `#`-imports flatten everything
  into one shared `Vec<Node>` (same reasoning `#` imports themselves already use).
- **`w-[auto]`/`h-[auto]`** reuse `Sizing::Hug` (no new sizing variant) — `NodeKind::Image`'s own
  `measure()` arm (`nowui-core/src/layout.rs`) scales the auto axis from the image's natural
  aspect ratio, but only when the *other* axis is `Sizing::Fixed`; a `Percent`/`Fill` other-axis
  is a known, documented scope limit (only resolved at arrange time, too late for this measure-
  pass computation). Both axes fixed stretches the image, aspect ratio not preserved.
- **Network images are never bundled** ("they are dynamic" — always a live GET, every time the
  owning node is created, never cached to disk or embedded at compile time). The fetch runs on a
  background `std::thread` (`nowui-runtime/src/network_image.rs`), off the render thread — a
  non-200 status, a transport error, and a decode failure are all reported the same way, through
  `NodeKind::Image::error`. `App::sync_network_image_loads` polls in-flight fetches once per
  redraw and starts a new one for any `http(s)://` node it finds still in the "loading" state
  (`decoded: None, error: None` — the same representation used while a local file simply hasn't
  been decoded yet, per `NodeKind::Image`'s own doc comment).
- **`loop`** (a bare style flag, `Style::loop_playback`) only affects an animated GIF's playback:
  once its frames have played through once, `loop` set wraps back to frame 0; unset, it just holds
  on the last frame. Meaningless for any other format, or a single-frame GIF. Frame advancement
  (`nowui-runtime/src/resolve.rs`'s `advance_image_animations`) is driven by real per-frame delay
  data from the decoded GIF (`Frame::delay_ms`), ticked forward by `FRAME_INTERVAL` every redraw —
  consistent with this engine's fixed-60fps, not event-driven, redraw loop.
- **`.nowdat` bundling** (opt-in, alternative to loading straight off disk): the `nowui-bundle`
  CLI packs a directory of image files into one `bundled.nowdat` sidecar archive —
  `cargo run -p nowui-bundle -- <assets-dir> <output>/bundled.nowdat`, keyed by each file's own
  **basename** (`logo.png`, not its full relative path — a deliberate simplification; two
  differently-nested files sharing a basename is a build-time error the tool refuses to bundle,
  not a silent collision). At runtime, `nowui-runtime`'s `bundled_assets.rs` looks for
  `bundled.nowdat` next to the running executable (`std::env::current_exe()`'s own directory,
  read once and cached for the process's lifetime) and tries a local `Image` source's basename
  against it *before* falling back to a disk read — so switching a shipped app from "assets on
  disk next to the exe" to "assets packed into one file" needs no change to `.nowui` source at
  all, just running `nowui-bundle` once as a packaging step and shipping the resulting
  `bundled.nowdat` alongside the executable instead of the raw asset files. This is the "reduce
  the compiled executable's own file size for an app with a lot of large images" half of the
  feature — unlike `#[nowui(view(...))]`'s `include_str!`-based `.nowui` *source* bundling, image
  bytes are never baked into the binary itself via `include_bytes!`.

**`Icon`** — a single icon from the embedded
[react-icons](https://github.com/react-icons/react-icons) library, referenced by the exact export
name react-icons itself uses (`FaUser`, `MdSettings`, `BsStar`, `IoRocket`, ...):

```nowui
Icon `FaUser` w-[48px] h-[48px]
Icon `FaHeart` w-[48px] h-[48px] line-color-[#dc2626] {onClick: state.likePost}
```

- **Sourced from `nowui-icons-gen`** (a dev tool, not shipped in any app binary), which parses
  react-icons' own generated JS modules — each set (`fa`, `md`, `bs`, ...) ships as one
  `GenIcon({"tag":"svg","attr":{...},"child":[...]})` call per icon, and that argument is itself
  valid JSON (an SVG-DOM-shaped tree) — no Node/npm dependency needed to read it. The tool
  reconstructs real SVG XML from that tree and packs every icon into one `.nowdat` archive
  (`nowui-icons/assets/icons.nowdat`, keyed by export name), regenerated via
  `cargo run -p nowui-icons-gen -- <extracted-react-icons-npm-package-dir> nowui-icons/assets/icons.nowdat [set...]`
  (sets default to `fa fa6 md bs io5` — the currently-bundled sets; add another set name to pull
  in more of the library later).
- **`nowui-icons`** embeds that archive via `include_bytes!` (compile-time, same "baked into the
  binary" precedent `#[nowui(view(...))]` sets for `.nowui` source) and does the actual
  SVG-to-RGBA rasterization via `resvg`/`usvg` (built on `tiny-skia`) — recoloring `currentColor`
  (react-icons' own SVGs set `fill="currentColor" stroke="currentColor"` at the root) to the
  resolved tint color textually before parsing, then rasterizing to a fixed `DEFAULT_RASTER_SIZE`
  square (128px) once per `(name, color)` pair, cached for the process's lifetime.
- **Not a `nowui-core` dependency** — `nowui-icons` pulls in `resvg`/`tiny-skia` transitively,
  which would violate `nowui-core`'s "no chumsky/tiny-skia/vello" hard rule. `nowui-runtime`'s
  semantic pass calls `nowui_icons::icon_frame(name, color)` and hands `nowui-core` only the
  already-rasterized `nowui_image::Frame` — the exact same shape `Painter::draw_image` already
  consumes for `Image`, so `NodeKind::Icon` paints through the identical path with zero new
  `Painter` methods.
- **Recolor**: `line-color-[#rrggbb]` (`fill-color` is an accepted alias — reads more naturally
  for the hover case below, both set the same `Style::line_color` field), falling back to
  `text-color` (whose own default is black). Baked into the rasterized pixels — but, unlike
  `Image`'s decode-once-at-build-time `source`, **re-resolved every redraw** from the node's
  *effective* style (`nowui-runtime`'s `resolve_icon_colors`, run after hover/focus/active
  variants and transitions are applied — see `App::apply_dynamic_styles`), so
  `hover:fill-color-[#ffff00]` and `hover:fill-color-[${state.hoverColorValue}]` (a `${state.
  path}` bracket inside a hover variant — resolved by `resolve_dynamic_styles`, which walks
  `variants.hover`/`focus`/`active`'s own `dynamic` map in addition to the base style's) both
  actually change what's painted while hovered. Looks wasteful (re-rasterizing every `Icon` every
  redraw) but isn't: `nowui_icons::icon_frame` caches by `(name, color)` for the process's
  lifetime, so an unhovered icon's own call is just a cache hit plus one small `Vec` clone.
- **`w-[auto]`/`h-[auto]`** work the same way `Image` already does (aspect-ratio-from-the-other-
  fixed-axis, reusing `Sizing::Hug` — see `Image`'s own bullet above) — an icon's rasterized frame
  is always square, so this mostly matters when only one axis is set.
- An unknown name (not in the currently-bundled sets) is a disclosed warning (`error` on the
  node, surfaced via `eprintln!` like every other semantic-pass warning), not a silent blank.
- Accepts the same generic event bindings every widget does — `onClick`, `onMouseDown`,
  `onMouseUp`, `onKeyDown`, `onKeyUp`, `onKeyPress` — no `Icon`-specific dispatch code exists;
  `apply_generic_bindings` doesn't special-case any widget kind.
- **`Dropdown`'s own caret and `Date`/`Time`/`DateTime`'s own closed-box icon** are drawn as
  `FaChevronUp` (open) / `FaChevronDown` (closed) from this same embedded library. **`TreeView`'s
  own disclosure indicator** uses a different, file-tree-conventional pairing instead —
  `FaChevronDown` (expanded) / `FaChevronRight` (collapsed) — since "closed points at its hidden
  content, open points down into it" reads better for a tree than the up/down-for-open/closed
  convention those other widgets use. All three glyphs are rasterized once at a fixed neutral
  color (`CHEVRON_COLOR`, gray-700) and stashed on `Ui::chevron_up`/`chevron_down`/`chevron_right`
  before the first paint, since `nowui-core` can't call `nowui-icons` itself. `nowui-runtime`'s
  `run_ast` does this for any app built via `run`/`run_path`; `nowui-designer` builds its `Ui`s by
  hand (`Semantic::build` directly, not `run_ast`) so it duplicates the same population itself, in
  both `chrome.rs`'s `Chrome::load` (the designer's own chrome, e.g. its project-explorer
  `TreeView`) and `preview.rs`'s `PreviewDoc::reload_with_overrides` (whatever `.nowui` file is
  being live-previewed) — each with its own local `CHEVRON_COLOR` copy, since `nowui-runtime`'s is
  private. A fixed color rather than each widget's own `text-color` is a deliberate scope
  simplification (matching precedent: a `Date`/`Time`/`DateTime` popup's own internals already use
  one fixed palette regardless of the widget's own styling) — a real per-widget-tinted chevron
  would mean rasterizing a fresh chevron per distinct color used in one tree, threaded per-node
  rather than once per app. Falls back to the old plain-square/triangle glyph if the relevant
  `Ui::chevron_*` field is `None` (e.g. a bare `nowui-core` test `Ui` with no runtime attached, or
  a harness that forgot to populate it — the "black box" symptom this dual population now fixes).
- **`TreeViewItem`'s own opt-in per-row icon** — `tree-icon-[FaFolder]` (any name from this same
  embedded library) draws that glyph at the row's own left edge, right after the disclosure
  triangle/checkbox and before the label (`Style::tree_icon`, `NodeKind::TreeViewItem::icon`).
  Unlike the fixed chevron glyphs above, this is genuinely per-row/per-app — resolved every redraw
  by `nowui-runtime`'s `resolve_tree_icons` (same `nowui_icons::icon_frame(name, color)` +
  process-lifetime cache shape `resolve_icon_colors` already uses for the standalone `Icon`
  widget), tinted with the row's own effective `text_color`. Empty (the default) draws nothing —
  a generic `TreeView` has no built-in notion of "this row is a folder"; an app decides that.
  `nowui-designer`'s own project explorer uses this for `FaFolder`/`FaFile` row icons.

**`scroll-h`/`scroll-v`** — clips overflow along that axis, mouse wheel pans it:

```nowui
Container scroll-v h-[160px] w-full border rounded gap-1 p-2 {
  Text `Row one`
  Text `Row two`
}
```

Thumb/track reuse `border-color` (falls back to neutral gray) at full/low alpha — no dedicated
`scrollbar-*` class family.

**`position-absolute`/`position-relative`** — containing block is the *nearest*
`position-relative`/`position-absolute` **ancestor**'s content box, same as real CSS: a plain,
unpositioned container in between is skipped over, however many levels deep. A layer with no
positioned ancestor at all falls back to its root's content box (CSS's initial containing block):

```nowui
Container position-relative w-[hug] h-[hug] {
  Text `Alerts`
  Container position-absolute top-[-8px] right-[-14px] bg-red-500 rounded-full px-[7px] {
    Text `3` text-white
  }
}
```

Implementation: `layout::arrange` threads a `containing_block: Rect` parameter down through the
whole recursive descent (`arrange` → `arrange_flow`/`arrange_grid` → `arrange`/`arrange_absolute`).
A node whose own `style.position` is `Relative` or `Absolute` swaps in its own content box
(`inner`) as the `containing_block` handed to its children; every other node just forwards the one
it was given. `arrange_absolute` resolves `left`/`top`/`right`/`bottom` against whichever rect it's
handed — it never has to know how many plain ancestors were skipped to reach it.

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

- **`nowui_runtime::run(window_title, entry, state)`** — `.nowui` source **bundled into the
  binary** via `#[nowui(view("/path.nowui"))]`. Use when shipping a real app: no `.nowui` file
  needed on disk at runtime. `window_title` becomes the OS window's title bar text.
- **`nowui_runtime::run_path(window_title, path, entry, state)`** — `.nowui` source loaded from
  disk at runtime. Use when iterating on a `.nowui` file without a rebuild, or one with `#` imports.
- Both default to `Backend::Gpu` (`vello`/`wgpu`) — call `run_with_backend`/`run_path_with_backend`
  (same arguments, plus an explicit `Backend` last) to pick `Backend::Cpu` instead.
- **the `nowui` CLI binary** (`nowui-runtime/src/main.rs`) — loaded from disk, `NoState`. Use for
  quickly previewing an arbitrary `.nowui` file with no Rust state at all.

### Bundling a `.nowui` file into the executable — `#[nowui(view("/path.nowui"))]`

Add the attribute alongside `#[derive(NowUiState)]` on your top-level state struct. The path is
resolved **relative to that crate's own `src/` directory** and embedded at compile time via
`include_str!` — the string literally becomes part of the binary, so nothing needs to exist on
disk at runtime. Then call `nowui_runtime::run(window_title, entry, state)` with no path argument at all:

add `#[nowui(view("/login.nowui"))]` to specify the root entry UI point
add `#[nowui(methods(sign_in))]` to specify the each method that will be used by the view

```rust
use std::process::ExitCode;
use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(view("/login.nowui"))]
#[nowui(root(AppState))]
#[nowui(methods(sign_in))]
pub struct AppState {
    username: String,
    password: String,
    rows: Vec<Row>,
}

impl AppState {
  pub fn sign_in(&self, app:&mut AppState, _event: &Event) {
        println!("username: {}, password: {}", self.username, self.password);
    }
}

#[derive(Default, Clone, NowUiState)]
#[nowui(root(AppState))]
#[nowui(methods(handle_me))]
pub struct Row {
    id:String,
    label:String,
}

impl Row {
    pub fn handle_me(&mut self, app:&mut AppState, _event:&Event){
    }
}

fn main() -> ExitCode {
    nowui_runtime::run( "Counter App", "App", AppState {
        username: "".to_string(),
        password: "".to_string(),
        rows: vec![Row { id: "x".to_string(), label:"x".to_string()}],
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
#[nowui(root(AppState))]
struct AppState {
    counter: Counter,
}

// Callable methods aren't auto-discovered from `impl Counter` — a derive
// macro can't see a separate impl block — so list them explicitly.
#[derive(Default, Clone, NowUiState)]
#[nowui(root(AppState))]
#[nowui(methods(increment, decrement))]
struct Counter {
    count: i64,
}

impl Counter {
    fn increment(&mut self, app:&mut AppState, _event: &Event) { self.count += 1; }
    fn decrement(&mut self, app:&mut AppState, _event: &Event) { self.count -= 1; }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/counter.nowui");
    nowui_runtime::run_path("Counter", nowui_file, "App", AppState::default())
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
    fn call(&mut self, path: &[&str], event: &mut Event, root: &mut dyn std::any::Any) -> bool;
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

#### `#[nowui(root(App))]` — why a handler method takes both `&mut self` *and* `&mut App`

Every generated handler arm calls the user's method as `self.the_method(root, event)`, i.e. two
mutable references: `self` (the struct the method is actually declared on — could be a small
nested struct like `Counter` or `Row`, not necessarily the app's top-level state) and `root` (the
*whole* app state, downcast from `call`'s `root: &mut dyn std::any::Any` parameter). This is what
lets a handler on a deeply-nested struct — a `Row` inside `Vec<Row>` inside `AppState`, say —
still reach and mutate sibling fields on `AppState` itself, not just its own fields.

The problem `root(...)` solves: the derive macro runs once per struct, in isolation — the
expansion for `Row` has no way to know that it will, at runtime, only ever be reached by
delegating down through `AppState.rows.<N>`. So it can't know what concrete type to
`downcast_mut::<T>()` `root` into before calling `self.handle_me(root, event)`. `#[nowui(root(App))]`
is exactly that missing piece of information, supplied explicitly:

```rust
#[derive(Default, Clone, NowUiState)]
#[nowui(methods(handle_me))]
#[nowui(root(AppState))]   // "when my methods are called, `root` is really an `&mut AppState`"
pub struct Row {
    id: String,
    label: String,
}

impl Row {
    pub fn handle_me(&mut self, app: &mut AppState, _event: &Event) {
        // `self`  -> this one Row (e.g. state.rows.3)
        // `app`   -> the entire top-level AppState, incl. other fields/rows
    }
}
```

The generated `call` arm does `let root = root.downcast_mut::<AppState>().expect(...); self.handle_me(root, event);` —
if the attribute is missing or names the wrong type, this `downcast_mut` fails at runtime with a
panic that names the mismatch, not a compile error, since the real type of `root` is only known
once `nowui-runtime` constructs the call chain.

Rules of thumb:

- **The actual top-level state struct — the one passed to `nowui_runtime::run`/`run_path` — never
  needs `root(...)` at all.** The attribute defaults to `Self`, which is already correct: at the
  top level, "the struct the method is declared on" and "the whole app's root state" are the same
  type. Adding `#[nowui(root(AppState))]` to `AppState` itself is redundant (though harmless,
  since it downcasts to the same type it already is) — omit it there.
- **Add `root(...)` only on a struct that is itself reached by delegation** — a field's type
  (`Counter` inside `AppState { counter: Counter }`), or a `Vec<T>` element type (`Row` inside
  `AppState { rows: Vec<Row> }`) — and only if that struct has its own `#[nowui(methods(...))]`
  that need to see back up to the root. A purely data-holding nested struct with no methods of
  its own needs no `root(...)` either.
- Name the *actual* top-level state type. If `AppState { counter: Counter }` is the struct handed
  to `run`/`run_path`, `Counter`'s attribute is `#[nowui(root(AppState))]` — not `root(Counter)`,
  and not some other ancestor if there are multiple delegation hops (`root(...)` always names the
  ultimate top-level struct, however many field-hops deep the method's own struct sits).
- SAFETY note (see the trait doc comment on `call` in `nowui-core/src/state.rs`): `root` and
  `self` alias the same memory whenever the handler's struct sits inside the root's own field
  tree, which — by construction — it always does. `nowui-runtime` constructs `root` via a raw
  pointer reborrow of the same state `call` is invoked on. This holds up fine for ordinary,
  non-overlapping field reads/writes, but don't write to the exact same field through both `self`
  and `root` inside one handler.

For a no-Rust-state file, use the CLI binary directly — `nowui_core::NoState` is a no-op impl
where every `get`/`set`/`call` returns `None`/`false`:

```sh
cargo run -p nowui-runtime -- path/to/file.nowui EntryLayoutName
```

---

## Editor tooling (`nowui-lsp` + `nowui-extension`)

`.nowui` syntax highlighting is provided by a real language server, not a static TextMate
grammar — `nowui-lsp` (a Rust binary, LSP over stdio) talks to `nowui-extension` (a VS Code
client, TypeScript/npm) via `vscode-languageclient`.

- **`nowui-lsp`** implements two things: `textDocument/semanticTokens/full` (the actual
  highlighting) and `publishDiagnostics` (parse errors, via `nowui_syntax::parse` — the same
  parser everything else uses, so a diagnostic here means the file genuinely won't build).
  Highlighting is driven by `tokenizer.rs`, a **standalone lexer**, deliberately not built on
  `nowui_syntax`'s AST — `ast::Node` carries no source spans at all (see its own module doc
  comment), and threading spans through every AST variant just for editor tooling would be a
  large, unrelated change to the parser crate. The tokenizer is single-pass, best-effort, and
  documents its own simplifications (no `${...}` sub-highlighting inside a backtick, no
  punctuation tokens, a heuristic — not the parser's real grammar-position rule — for
  telling a `variant:key` compound style token apart from a `{key: value}` binding's colon).
  `line_index.rs` converts its char-offset spans (and `chumsky::Simple<char>`'s parse-error
  spans — also char-offsets) into LSP's UTF-16-code-unit `Position`s.
  Depends on `nowui-syntax` directly (for `parse`) — this is compile-time-only editor tooling
  that ships in no NowUI app binary, so it doesn't participate in `nowui-core`'s "no chumsky"
  hard rule, which is specifically about the runtime *model* crate staying parser-agnostic.
  `TextDocumentSyncKind::FULL` (simplest correct option — re-tokenizing a whole `.nowui` file on
  every keystroke is cheap) — no incremental sync, no completion/hover/go-to-definition yet.
- **`nowui-extension`** is a thin client: `src/extension.ts` resolves the `nowui-lsp` executable
  (the `nowui.serverPath` setting, then `target/debug|release/nowui-lsp[.exe]` under an open
  workspace folder, then bare `nowui-lsp` on `PATH`) and starts a `LanguageClient` over stdio.
  `language-configuration.json` covers comment-toggling/bracket-matching (editing ergonomics, not
  highlighting — that's the server's job via semantic tokens). Not a Cargo workspace member; see
  its own `README.md` for the npm dev workflow (`npm install && npm run compile`, then F5 to
  launch an Extension Development Host).


# NowUI VSCode Extension

Build and install the vscode extension

for windows
```
cargo build --release -p nowui-lsp
cd nowui-extension
npm run package:win32-x64
npm run stage-lsp
code --install-extension bin/nowui-extension-win32-x64-0.1.0.vsix
```

for linux
```
cargo build --release -p nowui-lsp
cd nowui-extension
npm run package:linux-x64
npm run stage-lsp
code --install-extension bin/nowui-extension-linux-x64-0.1.0.vsix
```


Pipeline, end to end:

```text
.nowui file --chumsky parser--> AST --semantic pass--> node arena
   --layout solver (2-pass)--> computed rects --paint walk--> Painter calls
   --Backend::Gpu (vello/wgpu raster, default)--------> window pixels
   --Backend::Cpu (tiny-skia raster --> Pixmap --softbuffer)--> window pixels
```

Two properties are load-bearing and shape everything else in this document:

- **Retained, not immediate.** The arena persists across frames. A redraw re-walks the existing
  tree and re-paints it; it does not rebuild the tree from scratch, except where `if`/`for`
  dynamic regions explicitly re-expand a subtree because the state they depend on changed.
- **A fixed 60fps loop, by explicit design choice — not event-driven.** `App::about_to_wait`
  schedules `ControlFlow::WaitUntil` at a steady `FRAME_INTERVAL` and redraws unconditionally every
  tick, whether or not anything changed. This is a deliberate departure from this engine's earlier
  event-driven-only model (still visible in some older comments/gotchas below) — don't "fix" it
  back to on-demand-only without re-checking with whoever owns this decision.

---

## Internal Libraries and Dependencies

### Third-party crates (do not change without reason)

- **`chumsky`** (0.9) — parser combinators; builds the `.nowui` AST.
- **`tiny-skia`** (0.11) — CPU rasterizer (`Backend::Cpu`). Has **no text support** — glyphs come
  from `cosmic-text`, rasterized by `swash` and blitted pixel-by-pixel (see `nowui-render`'s
  module doc for why this means CPU-backend text never rotates with its node's transform).
- **`cosmic-text`** (0.12) — text shaping/layout, shared by *both* backends via `nowui-text`.
  Feeds shaped glyphs into `swash` (CPU) or straight into `vello` (GPU) — see `nowui-render-gpu`'s
  module doc for how a shaped glyph run maps onto `vello::Scene::draw_glyphs`.
- **`vello`** (0.9) — GPU 2D scene renderer (`Backend::Gpu`, the default), built on `wgpu`.
  Re-exports `wgpu`/`peniko`/`kurbo` as `vello::wgpu`/`vello::peniko`/`vello::kurbo` — use those
  re-exports rather than adding separate `wgpu`/`peniko`/`kurbo` workspace deps, so their versions
  can never drift out of sync with what `vello` itself was built against.
- **`pollster`** (0.3) — blocks on the one-time async `wgpu` adapter/device negotiation in
  `GpuSurfaceState::new` (called from `App::resumed`, itself synchronous) — the only blocking-async
  call anywhere in this codebase.
- **`winit`** (**0.30**) — windowing + event loop.
- **`softbuffer`** (0.4) — presents the rasterized `Pixmap` to the OS window, `Backend::Cpu` only.
- **`syn` / `quote` / `proc-macro2`** (2 / 1 / 1) — power the `#[derive(NowUiState)]` proc-macro.
- **`image`** (0.25) — png/gif/jpeg/bmp/webp decoding for `nowui-image`, the shared,
  renderer-agnostic `Image` widget preprocessing crate (no `nowui-core` dependency, same shape as
  `nowui-text`).
- **`ureq`** (3, rustls-only feature set — no cookies/json/multipart/native-tls) — the
  `nowui-runtime`-only, synchronous HTTP client behind `network_image.rs`'s background-thread
  fetch for `Image`'s `http(s)://` sources. Deliberately not `reqwest`/`tokio` — a plain blocking
  GET on its own `std::thread` needs no async runtime.
- **`resvg`/`usvg`** (0.47, `default-features = false` — no `text`/`system-fonts`, since every
  bundled icon is plain path/shape geometry with no text elements) — `nowui-icons`-only SVG
  parsing and rasterization for the `Icon` widget, built on `tiny-skia` (a *different* `tiny-skia`
  version than `nowui-render`'s own — resvg pulls in its own; the two never need to interoperate,
  since `nowui-icons` only ever exports plain `Vec<u8>` RGBA bytes across its own crate boundary).
  Not a `nowui-core` dependency — see the `Icon` widget section above for why.
- **`serde_json`** (1) — `nowui-icons-gen`-only, to parse the `GenIcon({...})` JSON blobs
  react-icons' own generated JS modules embed (see the `Icon` widget section above).

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
- **`nowui-text`** — `TextContext` (font database + glyph cache) and the cosmic-text
  shaping/measurement functions (`shape_text`, `measure`), shared by both `Painter` backends below
  so this logic exists exactly once regardless of how a backend rasterizes the glyphs it gets
  back. Pure cosmic-text — no `tiny-skia`, no `vello`/`wgpu`.
- **`nowui-render`** — the tiny-skia `SkiaPainter` implementation of the `Painter` trait
  (`Backend::Cpu`), plus the softbuffer presentation bridge.
- **`nowui-render-gpu`** — the vello/wgpu `GpuPainter` implementation of the `Painter` trait
  (`Backend::Gpu`, the default), plus `GpuSurfaceState` (owns the `wgpu::Surface`/`Device`/`Queue`
  and `vello::Renderer` tied to an on-screen window's lifetime, via `vello::util::RenderContext` —
  see its module doc for why an intermediate storage-capable texture + blit is needed rather than
  rendering directly into the swapchain image).
- **`nowui-runtime`** — the `#` import loader, the semantic pass (AST → arena, including dynamic
  `if`/`for` region expansion), the expression evaluator (`dynamic.rs`), the transition driver,
  and the winit `App<S: NowUiState>` (lib + a thin CLI binary `nowui`) that ties state,
  layout, and paint together every redraw. Owns `Backend` (`Cpu`/`Gpu`) and the
  `run`/`run_path`/`run_with_backend`/`run_path_with_backend` entry points.

### The one hard architectural rule

**`nowui-core` must never import `chumsky`, `tiny-skia`, or `vello`/`wgpu`.** The model stays
testable in isolation and the renderer stays swappable — `nowui-render` and `nowui-render-gpu`
are proof: two independent `Painter` implementations behind the same trait, neither known to
`nowui-core`. If you need syntax or render types in core, you're putting something in the wrong
crate. Dependency arrows point one direction only:
`nowui-syntax` / `nowui-render` / `nowui-render-gpu` → (never) `nowui-core` → (never) `nowui-runtime`.

### Architecture decisions (keep consistent with these)

- **Node arena, not a recursive owned tree:** flat `Vec<Node>` + `NodeId(u32)` indices, with
  **no parent pointers**. Deliberate — avoids borrow-checker fights, makes focus/hover references
  cheap. A node that needs its ancestor (e.g. a `MenuItem` closing its own `Menu`) can't walk up;
  the caller that already knows both ids (`App`, which owns the whole arena) does the work
  instead. Do not refactor into `struct Node { children: Vec<Node> }`.
- **Layers** = `Vec<Layer>`, each its own layout root, composited back-to-front. Hit-testing goes
  front-to-back (topmost layer wins).
- **`Painter` trait is the render boundary** (`fill_rect`, `stroke_rect`, `draw_text`,
  `push_clip`/`pop_clip`, `measure_text`, `push_transform`/`push_opacity`). Two independent impls:
  `SkiaPainter` (CPU, `nowui-render`) and `GpuPainter` (GPU/`vello`, `nowui-render-gpu`, the
  default — see `nowui_runtime::Backend`). Both mirror the same design for the clip/transform/
  opacity stacks: plain cumulative data (`Vec<Transform>`/`Vec<f32>` opacity), applied fresh as a
  parameter to each individual draw call — *not* pushed as nested render-target/layer state,
  because the paint walk's push/pop sequence doesn't nest as simple symmetric layers (a node can
  pop its own clip partway through painting its children while its transform/opacity stay active
  for what's painted after). `GpuPainter` is the one exception: it *does* use a real
  `vello::Scene::push_layer`/`pop_layer` for clips specifically, since `push_clip`/`pop_clip` are
  the one stack that's always properly nested — see that crate's module doc.
  A real, documented fidelity difference between the two: `SkiaPainter` blits glyphs as pixels, so
  text never rotates/scales/skews with its node's transform; `GpuPainter` draws glyphs as real
  transformable primitives via `vello::Scene::draw_glyphs`, so text *does* follow the active
  transform. This is intentional — a GPU-backend improvement, not backported to CPU.
  "Retained" refers to the tree, not cached draw commands — the paint pass re-walks the tree each
  redraw regardless of backend; don't add draw-command caching until profiling demands it.
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

- **Frame pacing is `about_to_wait`'s job, not `redraw`'s.** The engine runs a fixed 60fps loop
  (`FRAME_INTERVAL`): `App::about_to_wait` compares against `next_frame`, requests a redraw and
  advances `next_frame` by exactly `FRAME_INTERVAL` (not `now + FRAME_INTERVAL`, to avoid drift
  accumulating over a long session — except when the app genuinely fell behind, e.g. the window
  was minimized, in which case it resyncs to `now + FRAME_INTERVAL` instead of firing a catch-up
  burst), then reschedules `ControlFlow::WaitUntil(next_frame)`. `WindowEvent::RedrawRequested`
  calls `redraw()` unconditionally — no dirty-flag gate — since every tick redraws regardless of
  whether anything changed. Older code/comments describing an on-demand, `ControlFlow::Poll`-
  while-a-transition-is-active scheme are stale; transitions and delayed `onLoad` timers
  (`pending_on_load_timers`) still work exactly as before, they just no longer need to drive
  `ControlFlow` themselves, since it's always ticking now.
- **Delayed `onLoad` (`{onLoadDelay: ...}`) must fire *before* `refresh_dynamic_regions` in
  `redraw`, not after.** A delayed handler often mutates state an `if`/`for` branches on (e.g. a
  splash screen navigating away); firing it after that frame's region re-evaluation already ran
  means the branch flip lands one frame late — `App::fire_due_on_load_timers` is deliberately
  called first, before `Semantic::refresh_dynamic_regions`, for exactly this reason.
- **Diagnosing "the style value looks right but nothing on screen changed":** verify the
  *animated* (post-`Transitions::step`) value with a temporary `eprintln!`, not just the target —
  now that every frame redraws unconditionally, a stale-looking screen points at state/region
  resolution logic, not at a missed redraw the way it used to.

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
