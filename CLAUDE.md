# CLAUDE.md

Standing instructions for working in the NowUI repository. Read this before making changes.

## What NowUI is

A file-based, retained-mode UI toolkit for Rust with a custom Tailwind-flavored syntax.
UIs are described in `.nowui` files, parsed to an AST, expanded into a node arena, laid out
with a two-pass solver, and CPU-rasterized. The reference target is a login screen: dark top
bar, blue field, centered white card with username/password inputs and a SIGN IN button.

## Tech stack (do not change without reason)

- **Parsing:** chumsky 0.9
- **Rasterizer:** tiny-skia 0.11 — has NO text support; glyphs come via cosmic-text 0.12
- **Windowing/present:** winit **0.30** + softbuffer 0.4
- Rendering is **retained-mode** and **event-driven** (`ControlFlow::Wait`; render only when
  dirty, on `RedrawRequested` — never a continuous animation loop).

### winit version is load-bearing
The app harness uses `ApplicationHandler` + `run_app`, which live in `winit::application` /
`winit::event_loop` as of **0.30**. They do NOT exist on 0.29 or earlier (closure-based API).
Keep `winit = "0.30"` in `[workspace.dependencies]`. If a build fails with
`unresolved import winit::application`, the version was downgraded — fix the pin, not the code.

## Workspace layout and the one hard rule

```
nowui-syntax/    chumsky parser -> AST        (no core, no render deps)
nowui-core/      arena, Style, tailwind tokens, geometry, solver, paint walk, Painter trait,
                 NowUiState trait / StateValue / Event (reactivity interface)
nowui-macros/    #[derive(NowUiState)] proc-macro (reflection glue), re-exported by nowui-core
nowui-render/    tiny-skia SkiaPainter + softbuffer bridge
nowui-runtime/   loader (# imports), semantic pass, transitions driver, winit app (lib + binary
                 `nowui`); generic App<S: NowUiState> resolves values and dispatches events
examples/login.nowui
examples/components_demo.nowui   Dropdown, scroll-v, position-absolute/relative, # import
examples/demo.nowui   full showcase: z-index layering, floating Dropdown popup, transitions,
                       transforms, draggable Slider, ProgressBar
examples/counter.nowui + nowui-runtime/examples/counter.rs   reactivity end-to-end: a real
                       #[derive(NowUiState)] struct backing {value: ...} and {onClick: ...}
examples/widgets/BillingCard.nowui, StatCard.nowui   imported by components_demo.nowui / demo.nowui
examples/counter-app/   standalone workspace member (its own Cargo.toml, binary `counter-app`) —
                       same reactivity demo as examples/counter.rs, but as an independent crate
                       shaped after the original App/Counter design sketch (see its main.rs for
                       the two deliberate deviations from that sketch: Clone not Copy, &mut self
                       not by-value self). `cargo run -p nowui-counter-app`.
```

**Hard rule: `nowui-core` must never import `chumsky` or `tiny-skia`.** The model stays
testable in isolation and the renderer stays swappable. If you need syntax or render types in
core, you're putting something in the wrong crate. Dependency arrows point one direction only.

## The NowUI language

Colon-delimited, brace-nested. NOT whitespace-sensitive. `//` line comments allowed.

Fixed widget line order — enforce this order in the grammar:
```
Kind  arg=value...  `string`...  style-[value]...  { bindings }  { children }
```

- Backtick string literals carry `${var}` interpolation — `${name}` or a dotted state path like
  `${state.counter.count}` (`nowui-syntax`'s `interp()` parses `.`-separated idents, not just a
  single one), mixed freely with literal text in the same backtick (`` `Count: ${state.counter.count}!` ``,
  unlike the style-bracket case below). Resolved at RUNTIME, not parse time, so the retained tree
  can re-resolve without re-parsing: the semantic pass converts each backtick's `Template` (parser
  AST type) into `nowui_core::Template`/`TemplatePart` (`Lit`/`Var`, the runtime-side type — kept
  separate because `nowui-core` can't depend on `nowui-syntax`, the hard rule below) and stores it
  on `Node::templates`, index-aligned with the widget's original backticks, but only when at least
  one of them actually contains a `${...}` — an all-literal node leaves `templates` empty and pays
  no extra cost. `nowui-runtime`'s `App::resolve_templates` (called every redraw, alongside
  `resolve_values`) re-renders each template against the live `NowUiState` and writes the result
  back into whichever field that backtick built (`Text.content`, `Button`/`Checkbox.label`,
  `TextInput.label`/`placeholder`, `Dropdown.placeholder`/`options`) via `apply_resolved_templates`
  — keep its index mapping in sync with `semantic.rs`'s `primitive()` if either changes. An empty
  `` `` `` is still significant — preserve it; it holds a positional slot (e.g. TextInput label
  vs placeholder).
- A style bracket value can *also* be a `${var}` interpolation (`w-[${state.myWidth}]`) — but
  only when the whole bracket is the interpolation; `"10${x}px"` (mixed literal+var) is not
  supported. The parser doesn't need to know about this at all — `${...}` is just more raw
  bracket-value text, already captured verbatim by the existing "anything but `]`" value grammar
  (no grammar change was needed for this). The semantic pass's `dynamic_var_path` detects it
  before attempting to parse the value as a literal, and records the dotted path on
  `Style::dynamic` (keyed by the style key, e.g. `"w"`) *instead of* setting the actual field —
  which is therefore left at whatever it already was (its default, or an earlier class's value).
  Unlike `Node::value_path`/`events` (see "Reactivity" below, now wired to a live `NowUiState`),
  `Style::dynamic` is still unresolved — a separate, still-inert mechanism, not yet connected to
  the state system, out of scope unless asked for.
- Styles are generic `key-[value]` tokens or bare flags (`grid`), PLUS compact Tailwind-scale
  classes (`p-4`, `bg-blue-500`, `grid-cols-3`) where the whole thing is one bare key with an
  empty value (see the Tailwind vocabulary section below — the parser doesn't distinguish these
  cases, it's all still "a key, optionally a bracket value"). The parser keeps raw pairs as
  `(String, String)`; resolving them into the `Style` struct happens in the semantic pass,
  which is also where unknown-key/unsupported-variant warnings belong. Keep the parser dumb.
- An optional `variant:` prefix on a style key (`hover:bg-blue-600`, `sm:w-[440px]`) is folded
  into the key string itself at parse time (still just a string — see the parser gotcha below)
  and split back out in the semantic pass into `Style::variants`.
- `layout: Name(params) { ... }` DEFINES a reusable, parameterized widget; `Name arg=value`
  USES it. Custom widgets and layouts are the same mechanism, expanded before layout solving.
  Args are named. Guard expansion with a depth cap against recursive definitions.
- Sizing values: `w-[fill]`, `w-[fill-2]` (weight 2), `w-[hug]`, `w-[440px]` are NowUI's own
  (non-Tailwind) sizing primitives — keep them working as-is. Tailwind's own `w-4`, `w-1/2`,
  `w-full` etc. resolve to `Sizing::Fixed`/`Sizing::Percent` instead (see below).
- `# relative/path.nowui` is a whole-file import: only valid at the top level, between
  `layout:` defs. `nowui-syntax` just parses it into `Node::Import { path }`; it's
  `nowui-runtime/src/loader.rs` that does the actual I/O — reads the file, resolves `path`
  relative to the *importing* file's own directory (not the CWD), inlines its top-level nodes
  in place, and recurses. A canonical-path `visited` set dedupes diamond imports (two files
  importing the same third file) and breaks cycles (`A` imports `B` imports `A`) for free — see
  `main.rs`, which calls `loader::load_and_resolve` instead of `nowui_syntax::parse` directly.

## Tailwind CSS v4 utility vocabulary (what's supported, what's deliberately not)

The style system supports most of Tailwind v4's *static* utility vocabulary — see
`nowui-core/src/tailwind.rs` (design tokens: spacing/color-palette/font-size/font-weight/
radius/duration/easing/breakpoint lookups, pure functions, no parser/renderer deps) and
`nowui-runtime/src/semantic.rs` (`apply_exact`/`apply_prefixed`: dispatches a style key to the
right `Style` field). Covered: spacing/sizing (`p-*`/`m-*`/`gap-*`/`w-*`/`h-*`, fractions like
`w-1/2`, `w-full`), the full 22-family x 11-shade color palette (`bg-*`/`text-*`/`border-*`),
typography (`text-{size}`, `font-{weight}`, `leading-*`, `tracking-*`), flexbox (`row`/`col`/
`row-reverse`/`col-reverse`, `items-*`, `justify-*`), CSS grid (`grid`, `grid-cols-*`,
`grid-rows-*`, `col-span-*`, `row-span-*`), borders + **per-corner** radius (`rounded-*`, see
below), `opacity-*`, 2D transforms (`translate-x/y-*`, `scale*`, `rotate-*`, `skew-x/y-*`),
transitions (`transition`, `duration-*`, `ease-*`, `delay-*`), positioning (`position-static`/
`position-relative`/`position-absolute`, `left-*`/`right-*`/`top-*`/`bottom-*`), scrolling
(`scroll-h`/`scroll-v`), and `hover:`/`focus:`/`active:` plus responsive `sm:`/`md:`/`lg:`/`xl:`/
`2xl:` variants.

**Explicitly out of scope** (don't half-implement these — either do them properly with the
underlying state/rendering model they need, or leave them as unknown-key warnings):

- `dark:`, `group-*`/`peer-*` — no theme or group/peer-state model exists anywhere in this
  engine to drive them. `hover:`/`focus:`/`active:` work because the runtime already tracks
  cursor position, focus, and mouse-down; responsive variants work because window resize is
  already tracked. Don't add `dark:` support without first designing an actual theme system.
- Stacked variants (`sm:hover:x`) — only a single `variant:` prefix is parsed.
- 3D transforms, filters/backdrop-filters, box-shadow, `@keyframes` animations — the renderer
  is a 2D CPU rasterizer with no shadow/blur pipeline and no keyframe/animation-curve concept
  beyond the single-property `transition` interpolation described below.
- CSS Grid beyond fixed/auto/fr tracks + row-major auto-placement with span: no named lines,
  `minmax()`, `auto-fit`/`auto-fill`, or dense packing.
- A `display: grid` container has **no intrinsic Hug width/height of its own** (its `fr` tracks
  only claim space once the container already has a definite size) — same as real CSS. Give it
  an explicit `w-full`/`w-[…]` (and height, if needed); don't expect Hug to "shrink to content"
  for a grid container the way it does for a flow container.

## Widgets

- **`Checkbox`** (`Checkbox` followed by a label backtick, then styles) — toggles on click (`App::handle_click` in
  `nowui-runtime/src/app.rs` flips `NodeKind::Checkbox.checked` directly; no external callback
  needed since it's self-contained state). Styleable: `bg` fills the box, `border-color` (falls
  back to `text-color`) strokes it, `rounded-*`/`radius` rounds it (box and the checked-mark both
  use `style.radius`), `text-color` is also the checked-mark fill and label color.
- **`Dropdown`** (`Dropdown` followed by a placeholder backtick, then one backtick per option,
  then styles, then an optional `{value: state.path}` binding) — the first backtick is the
  placeholder, every backtick after it is an option (reuses the existing positional-string-args
  mechanism; no array/list `BindValue` was added for this). Styleable: `border-color`/`rounded`/
  `radius` on the box, `bg`/`text-color` on both the box and the popup panel.
  **The open option list floats over the page** — it's collected during the main paint walk
  (`paint_node` pushes the node's id onto a `popups: &mut Vec<NodeId>` instead of drawing it
  inline) and drawn last, in `paint::paint_dropdown_popup`, once every layer has painted and no
  ancestor `push_clip` is active — so it overlays everything, unclipped, and (critically)
  `layout::measure` never adds the option rows to the node's own height, so opening it does NOT
  push later siblings down. Consequence: the popup lives *outside* the node's own `computed`
  rect, so it is NOT reachable through the normal rect-based `Ui::hit_test`. Clicks are checked
  against it explicitly and first: `App::find_open_dropdown_popup_at` (mirroring
  `paint_dropdown_popup`'s exact placement math — keep the two in sync) before falling back to
  `Ui::hit_test` for everything else. Clicking the closed box toggles it open (`handle_click`);
  clicking inside an open popup selects that option and closes it (`select_dropdown_option`);
  clicking anywhere else closes every *other* open dropdown (`close_other_dropdowns`) since there's
  no independent outside-click-detection mechanism. Selecting an option writes the option string
  back to `value_path` via `App::write_back_value` (two-way binding, like `Slider`/`Checkbox`).
  Box-height formula lives in one place, `nowui_core::dropdown_metrics(font_size)`, shared
  by `layout::measure` (sizing), `paint::paint_dropdown_popup` (placement), and `app.rs`'s hit
  math — keep all three sharing it; don't duplicate the formula.
- **`scroll-h`/`scroll-v`** — clips overflow along that axis and lets the mouse wheel pan it, in
  the natural/inverted direction (wheel-away-from-user moves the *view* down — `app.rs`'s
  `MouseWheel` handler does `scroll_offset -= delta`, not `+=`; if this ever looks backwards on a
  given platform's wheel convention, flip that one sign, don't restructure anything else).
  `Node::scroll_offset` (runtime-only, never touched by the solver) is applied as a plain
  screen-space X/Y shift to children's rects in `arrange_flow`/`arrange_grid`, independent of
  flex direction. `Node::content_size` (filled by `arrange` every solve) is the union content
  extent, used to clamp the offset and size the thumb. Wheel routing walks `Ui::hit_test_chain`
  (root-first ancestor chain) from the cursor's deepest hit outward, so a scrollable list nested
  inside another scrollable area gets the wheel event, not its ancestor. Styleable: the thumb
  reuses `border-color` (falls back to a neutral gray) and the track is that same color at low
  alpha — there's no dedicated `scrollbar-*` class family, by design, to avoid multiplying the
  style surface for a purely-cosmetic detail.
- **`position-absolute`** containing block is always the *direct* parent's content box — real
  CSS walks up to the nearest ancestor with a non-static `position`, which this doesn't do (see
  `arrange_absolute` in `nowui-core/src/layout.rs`). `position-relative` only nudges the element
  itself via `left`/`top`/`right`/`bottom`; it doesn't (yet) get walked for `Absolute` containing
  blocks beyond one level.
  **`Absolute` children also escape their direct parent's own clip when painting** (a badge
  pinned outside its box via a negative offset, e.g. `top-[-10px]`, must not get its overflow cut
  off by the very box it's escaping). `paint_node` splits `node.children` into in-flow and
  `Absolute` groups; only the in-flow group is painted between that node's own
  `push_clip`/`pop_clip`; `Absolute` children paint afterward, still subject to whatever *further*
  ancestor clip is active on the painter's stack, just not this one level. Regression test:
  `paint::tests::absolute_child_paints_outside_parents_own_clip`. If you touch child painting
  again, keep this split — don't go back to one `push_clip` wrapping every child unconditionally.
- **`z-index-[N]`/`z-index-N`** reorders *paint* order only, among sibling nodes — it never
  changes layout/hit-testing/measured position, just which sibling's pixels end up on top.
  `paint_node` stable-sorts a *copy* of `node.children` by `style.z_index` before painting them
  (`children.sort_by_key`; stable, so equal-z-index ties keep source order) — layout still walks
  `node.children` in its original, unsorted order everywhere else. There is no cross-subtree
  stacking-context concept: a z-index only competes with its own siblings, the same as real CSS
  `z-index` without `position` establishing a new stacking context at every level.
- **`Slider`** (`Slider styles... {value: N}`) — a draggable `0.0..=1.0` value. `{value: N}` as a
  literal number (0..=100) sets the starting position; a `state.*` path there is stored on
  `Node::value_path` (see below) for once reactivity lands. Dragging is real, intrinsic
  interaction — `App` in `nowui-runtime/src/app.rs` tracks `dragging_slider: Option<NodeId>`
  across `MouseInput`/`CursorMoved`, computing the value from cursor-x against the track rect
  (`set_slider_value_from_cursor`) — clicking the track jumps the thumb there, then drags from
  it. Styleable: `text-color` is the track-fill/thumb color, `border-color` is the empty-track
  color (and strokes the thumb, if set); no dedicated `slider-*` classes. Geometry
  (`nowui_core::slider_metrics`) is shared by `layout::measure`, `paint`, and the drag math — keep
  them in sync. `Sizing::Hug` falls back to `DEFAULT_CONTROL_WIDTH` (160px, like a bare
  `<input type="range">`) since there's no text content to hug against.
- **`ProgressBar`** (`ProgressBar styles... {value: N}`) — same styling convention and geometry
  helper as `Slider`, but read-only: no drag, no `App` interaction state at all.
- **Generic `value`/event bindings** (`Node::value_path: Vec<String>`, `Node::events:
  HashMap<String, Vec<String>>`) — every widget, primitive or custom-layout-use, can carry a
  `{value: state.path}` binding (read by `Text`/`Checkbox`/`Dropdown`/`Slider`/`ProgressBar`;
  harmlessly unused by anything else — deliberately excludes `TextInput`, which has no cursor/
  IME system yet to drive from state) and any of `onClick`/`onMouseMove`/`onMouseDown`/
  `onMouseUp`/`onKeyPress`/`onKeyDown`/`onKeyUp`/`onResize` (`nowui_core::EVENT_BINDING_KEYS` —
  add a new event there, not as a one-off special case). Extracted generically by `semantic.rs`'s
  `apply_generic_bindings`, called once per expanded node. Dispatched every frame by
  `nowui-runtime`'s `App<S: NowUiState>` — see "Reactivity" below. `Slider`'s dragging is a
  completely separate, already-real mechanism — don't confuse the two when extending either.

## Reactivity (`state.*` bindings bound to a live Rust struct)

- **The boundary is `nowui_core::NowUiState`** (`nowui-core/src/state.rs`): `get(&self, path:
  &[&str]) -> Option<StateValue>`, `set(&mut self, path, value) -> bool`, `call(&mut self, path,
  event: &Event) -> bool`. `nowui-core` only defines this interface (plus `StateValue`,
  `Event`/`EventKind`, and the no-op `NoState`) — no reflection, no macros, keeping the hard rule
  (no chumsky/tiny-skia) intact.
- **`#[derive(nowui_core::NowUiState)]`** (`nowui-macros/src/lib.rs`, re-exported through
  `nowui-core` so consumers only ever depend on one crate) generates the `get`/`set`/`call`
  string-path dispatch for a named-field struct. Leaf fields (`String`, `bool`, any integer/float,
  normalized to `f64`) get direct get/set arms; any other field type is assumed to itself derive
  `NowUiState` and gets a delegating arm (`counter: Counter` → `Counter` also derives it) — this is
  a syntactic guess, so a wrongly-typed field just fails with a normal trait-not-implemented error.
  **Callable methods are never auto-discovered** — a derive macro can't see the struct's separate
  `impl` block — so list them explicitly: `#[nowui(methods(increment, decrement))]`, each existing
  as `fn NAME(&mut self, event: &nowui_core::Event)` in a plain `impl` block written as usual.
- **Every `.nowui` binding path is rooted at the literal `state` segment**
  (`["state", "counter", "count"]`), but a `NowUiState` impl is rooted at its own struct's fields —
  `nowui-runtime/src/app.rs`'s `state_subpath` strips that leading segment before crossing the
  reflection boundary. Don't skip this when adding new call sites; every `state.get`/`set`/`call`
  invocation from `app.rs` goes through it.
- **Read path**: `App::resolve_values`, called once per redraw before `layout::solve`, walks every
  node with a non-empty `value_path`, resolves it against `self.state`, and writes the result into
  the specific widget field: `Text.content` (via `display_string`, which renders any `StateValue`
  variant), `Checkbox.checked`, `Dropdown.selected` (matched by string against `options`),
  `Slider.value` and `ProgressBar.value` (both scaled from a `0..=100` `StateValue::Number`). A
  `Slider` mid-drag (`self.dragging_slider == Some(id)`) is skipped so a stale read can't fight the
  live gesture. `App::resolve_templates` is the same idea for inline `${state.path}` interpolation
  inside a backtick (`` `Count: ${state.counter.count}` ``) rather than a `{value: ...}` binding —
  see the backtick-template bullet above; a node can carry both a `value_path` and `templates`.
- **Write path**: `App::write_back_value(id, value)` calls `self.state.set(...)` — wired into
  `handle_click` (Checkbox toggle), `select_dropdown_option` (Dropdown pick), and
  `set_slider_value_from_cursor` (Slider drag), so all three are genuinely two-way bound.
  `App::dispatch_event(id, name, kind, key)` calls `self.state.call(...)` for the node's bound
  path, if any, and marks `self.ui.dirty` when the handler ran (a callback mutating state almost
  always needs a redraw). Wired into every `EVENT_BINDING_KEYS` entry: `onClick` (in
  `handle_click`), `onMouseDown`/`onMouseUp` (`MouseInput`), `onMouseMove` (`CursorMoved`),
  `onKeyDown`/`onKeyPress`/`onKeyUp` (a new `WindowEvent::KeyboardInput` arm, targeting
  `self.ui.focus`, extracting the key via `logical_key.to_text()` falling back to
  `format!("{:?}", ...)`), `onResize` (broadcast to every node in the tree, since it's a
  window-level event with no single target — see `dispatch_event_broadcast`).
- **`App<S: NowUiState>` is now generic** — `nowui-runtime` is a lib (`src/lib.rs`) exposing
  `run<S: NowUiState + 'static>(path, entry, state)`, plus a thin CLI binary (`main.rs`) that calls
  it with `nowui_core::NoState` (a no-op impl — every `get`/`set`/`call` returns `None`/`false`, so
  existing `.nowui` files with no Rust state keep working exactly as before). Your own binary
  depends on `nowui-runtime` as a library and calls `nowui_runtime::run` with a real
  `#[derive(NowUiState)]` struct — see `nowui-runtime/examples/counter.rs` +
  `examples/counter.nowui` for the full pattern (`cargo run -p nowui-runtime --example counter`).
- **Control flow is unchanged and deliberately so**: winit's `ApplicationHandler` + `run_app` +
  `ControlFlow::Wait` still owns the loop. State mutations from callbacks just mark `self.ui.dirty`
  and flow through the existing event-driven redraw path — there is no user-facing poll loop
  (`inputs()`/`events()`/`cleanup()`), and none should be added without discussing it first.

## Architecture decisions (keep consistent with these)

- **Node arena, not a recursive owned tree:** flat `Vec<Node>` + `NodeId(u32)` indices. This
  is deliberate — it avoids borrow-checker fights and makes parent/focus references cheap.
  Do not refactor into `struct Node { children: Vec<Node> }`.
- **Layers** = `Vec<Layer>`, each its own layout root, composited back-to-front. Hit-testing
  goes front-to-back (topmost layer wins). Decide event pass-through between layers explicitly.
- **Painter trait is the render boundary** (`fill_rect`, `stroke_rect`, `draw_text`,
  `push_clip`/`pop_clip`, `measure_text`). tiny-skia is one impl. "Retained" refers to the
  tree, not cached draw commands — the paint pass re-walks the tree each redraw. That's fine;
  don't add draw-command caching until profiling demands it.
- **Solver** is a compact two-pass measure-then-distribute (a flex approximation: no min/max or
  wrap). It also does grid (`Display::Grid`: fixed/auto/fr tracks, row-major auto-place with
  span — no named lines/`minmax()`/`auto-fit`/dense packing). It's swappable for `taffy` later
  without touching the arena or painter — keep that boundary clean.
- **`Style::radius` is `Edges`, not `f32`** — four independent corner radii, reusing `Edges`'s
  1/2/3/4-value CSS shorthand but as corners: `top`=top-left, `right`=top-right, `bottom`=
  bottom-right, `left`=bottom-left (clockwise from top-left, matching real CSS `border-radius`
  corner order and its 2-value diagonal-pair shorthand). `Painter::fill_rect`/`stroke_rect` take
  `radius: Edges` accordingly — `nowui-render`'s `rounded_path` builds one quadratic per corner.
- **softbuffer bridge:** tiny-skia `Pixmap` is RGBA8 premultiplied; softbuffer wants `0RGB`
  u32. Fill an opaque background first (so premultiplied == straight), then pack
  `(r<<16)|(g<<8)|b`.

## Parser gotchas (learned the hard way — don't regress these)

1. **Comments:** whitespace skipping must also eat `//` line comments. Use the `pad()` helper
   at structural boundaries, not bare `.padded()`.
2. **Style key must be `ident ('-' ident)*`**, where `-` only joins when followed by a key
   char (lookahead). Otherwise `p-[..]` folds the `-` into the key and bare `grid` over-eats.
   Build the key String with `.then(...).map(...)` — do NOT use chumsky `.chain()`; its two
   `Chain` impls make `T` ambiguous (`cannot infer type`).
3. **Style `value`** takes an optional leading `-` then `[...]`, so the dash between key and
   bracket is consumed on the value side.
4. **`{ }` ambiguity (the important one):** bindings `{key: value}` and child blocks
   `{ Widget... }` both open with `{`. Do NOT solve this with manual `{`-lookahead + `.rewind()`
   — it regresses the binding tests. Solve it with a `Trailer` enum and an ordered
   `choice((bindings, child_block)).or_not()` in `node()`: bindings tried first, falls through
   to child block on failure. Consequence: a widget can't have BOTH bindings and children —
   that's acceptable (inputs have bindings, containers have children). If chumsky 0.9 won't
   backtrack because `delimited_by` consuming `{` counts as commitment, disambiguate on content
   (`{ ident :` means bindings) instead.
5. **Bare-flag styles vs. the next sibling's `kind` (real bug, hit in `login.nowui`):** a bare
   style flag (`grid`, `row`) and a widget `kind` ident are both plain identifiers with nothing
   syntactically between them but whitespace. When one node's style list has no bracketed value
   left to terminate it, `style().repeated()` will happily eat the *next* sibling's `kind` as
   one more bare flag — e.g. two sibling `Text` nodes where the first ends its style list with
   no bracketed value silently merge into one node, then fail deeper in with a confusing
   `expected '/', '}', '{'` error nowhere near the real cause. Fixed by requiring a style key's
   first character be lowercase or `_` (`key_start` in `style()`), matching the codebase-wide
   convention that widget kinds are Capitalized and style/binding keys are lowercase. Do not
   loosen that first-char check without another way to terminate a bare-flag style list.
   Regression test: `sibling_nodes_dont_swallow_next_kind_as_bare_style` in
   `nowui-syntax/src/lib.rs`. Also remember gotcha #1 wasn't fully applied everywhere: brace
   delimiters need `pad()` (comment-aware) on *both* sides and on every `repeated()` element, not
   plain `.padded()` — padding only the brace token itself skips whitespace around it but still
   breaks on a `//` comment right after `{`.
6. **`key_char` includes `/` and `.`** (added for Tailwind fraction/decimal-scale classes like
   `w-1/2`, `py-3.5`). Without them the style key stops mid-token and the leftover `/2`/`.5`
   fails to parse as anything (a `/` even gets misread as the start of a `//` comment by `pad()`,
   producing a confusing "expected '/'" error far from the real cause — same failure shape as
   gotcha #5). Neither character can appear as a key's *first* character (`key_start` still
   requires lowercase/`_`), so this doesn't reopen the sibling-swallowing ambiguity.

## Runtime gotchas (learned the hard way — don't regress these)

- **`request_redraw()` from inside `RedrawRequested` is not a reliable way to keep animating.**
  The first version of the transition driver called `request_redraw()` at the end of `redraw()`
  whenever a transition was still in-flight, staying on `ControlFlow::Wait` otherwise. On Windows
  this visibly stalled: hovering a `transition` button would compute the correct target color but
  the pixel never moved, because the self-requested redraw got coalesced with the current one
  instead of scheduling a genuinely new frame. Fixed by driving `ControlFlow` directly —
  `event_loop.set_control_flow(ControlFlow::Poll)` while `Transitions::any_active()` is true,
  back to `ControlFlow::Wait` once it isn't (see `App::redraw` in `nowui-runtime/src/app.rs`,
  which now takes `&ActiveEventLoop` for exactly this). Don't revert to a `request_redraw()`-only
  scheme for animation-driven redraws.
- **Diagnosing "the style value looks right but nothing on screen changed"**: `compute_effective`
  computing the correct target is necessary but not sufficient — verify with a temporary
  `eprintln!` of the *animated* (post-`Transitions::step`) value, not just the target, and check
  the actual redraw count (a suspiciously low one, e.g. 3 redraws across a second of mouse
  movement, means frames aren't being pumped — see the `ControlFlow` gotcha above before
  suspecting the style-resolution logic).

## Build & test discipline

- Fix and build crate-by-crate in dependency order: **syntax → core → render → runtime**.
  Errors in higher crates often clear once lower ones compile.
- `cargo test -p nowui-syntax` — parser, incl. tests against `examples/login.nowui`. Fast, no
  window. This is the primary regression net; add a test for every grammar change.
- `cargo test -p nowui-core` — solver on hand-built arenas. No display needed.
- `cargo run -p nowui-runtime -- examples/login.nowui App` — opens the window.
- `cargo run -p nowui-runtime --example counter` — reactivity end-to-end: a real
  `#[derive(NowUiState)]` struct driving `{value: ...}`/`{onClick: ...}`.

When you change the grammar, add or update a `nowui-syntax` test in the same commit. When you
change the solver, add a hand-built-arena assertion in `nowui-core`.

## Solver gotchas (learned the hard way — don't regress these)

- **Pass 2 (`arrange`) must reuse pass 1 (`measure`)'s sizes, never re-derive them.** There used
  to be a separate `intrinsic_main` helper in `nowui-core/src/layout.rs` that re-estimated a
  Hug-sized child's extent from scratch during `arrange`, and for anything that wasn't a
  `Text`/`Button` node it fell back to a flat `120.0` width / `font_size * 1.6` height — so any
  `Container` sized `Hug` on an axis (e.g. a card whose height should hug its stacked children)
  collapsed to that flat default instead of the real content height. This was invisible with the
  old placeholder-bar text (a collapsed card just looked like an empty white box) but became
  obvious the moment real text landed: the whole card rendered as a single sliver. Fixed by
  having `measure()` memoize every node's `Size` into a `HashMap<NodeId, Size>` (`sizes` in
  `solve()`), threading `&sizes` through `arrange()`, and reading `sizes[&c]` instead of calling
  a re-estimate. If you touch the solver again, do not reintroduce a from-scratch estimate in
  pass 2 — look up the memoized pass-1 result instead.

## Roadmap order (each step runnable before the next)

1. ✅ Parser green — `cargo test -p nowui-syntax` passes, including `examples/login.nowui`.
2. ✅ Solver green on hand-built arenas — `cargo test -p nowui-core` passes.
3. ✅ Boxes on screen — `cargo run -p nowui-runtime -- examples/login.nowui App` opens the
   window and renders the reference layout (dark top bar, blue field, centered white card)
   correctly.
4. ✅ Real text (cosmic-text) — `draw_text`/`measure_text` shape and rasterize actual glyphs
   (see above); verified visually against `examples/login.nowui` (Login/Login Form/Username/
   Password/placeholders/SIGN IN all render with real anti-aliased text, not placeholder bars).
5. Input + focus — `Checkbox` toggles and `Dropdown` open/select are wired (`App::handle_click`);
   `onClick` and the other `EVENT_BINDING_KEYS` now dispatch for real (see step 6). `TextInput`
   cursor/selection/IME is still the remaining piece of this step.
6. ✅ Bind `state.*` to a live state object (reactivity) — `nowui_core::NowUiState` +
   `#[derive(NowUiState)]` (`nowui-macros`) + generic `App<S: NowUiState>` in `nowui-runtime`; see
   the "Reactivity" section above and `examples/counter.nowui` /
   `nowui-runtime/examples/counter.rs`. `${var}` style-bracket interpolation (`Style::dynamic`) is
   still unwired to this system — a separate, still-inert mechanism, out of scope until asked for.
7. Per-layer pixmap caching: re-rasterize only dirty layers, then composite.

## Style conventions

- Keep the parser free of semantics; keep `nowui-core` free of parser/renderer types.
- Prefer small, focused commits that keep the relevant `cargo test -p ...` green.
- Don't introduce a GUI framework, an async runtime, or a GPU backend without discussing it —
  the point of this project is a from-scratch CPU toolkit.