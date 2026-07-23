//! `#[derive(NowUiState)]` — generates the string-path reflection glue
//! (`nowui_core::NowUiState::get`/`set`/`call`/`to_state_value`) a live
//! app-state struct needs to back `.nowui` `{value: state.foo.bar}`
//! bindings, `{onClick: state.foo.bar}` callbacks, and `for x in
//! state.rows { ... }` loops. See `nowui-core/src/state.rs` for the trait
//! itself and `CLAUDE.md` for the end-to-end reactivity design.
//!
//! Scope (deliberately small — extend the match statements below if you
//! need more):
//!   * Named-field structs only (no tuples/units/enums).
//!   * Leaf field types: `String`, `bool`, and any integer/float primitive
//!     (`Int`/`Float` kept distinct — see `StateValue`'s doc comment).
//!   * `Vec<T>` fields become `StateValue::List` — a `for`-loop's iterable.
//!     If `T` is a leaf scalar, each element is that scalar `StateValue`;
//!     otherwise `T` is assumed to itself derive `NowUiState`, and each
//!     element is `T::to_state_value()` (a `StateValue::Object` snapshot of
//!     its fields), so `${item.field}` can resolve per-iteration. Read-only
//!     either way — no `.nowui` syntax writes back into a whole list yet.
//!   * Any other (non-`Vec`) field type is assumed to itself implement
//!     `NowUiState` and gets a *delegating* get/set/call/to_state_value arm
//!     (e.g. `counter: Counter` where `Counter` also derives `NowUiState`)
//!     — this is a syntactic guess (derive macros can't see trait bounds),
//!     so a wrongly-typed field just fails to compile with a normal
//!     trait-not-implemented error.
//!   * Callable methods aren't discovered from the struct's `impl` block —
//!     derive macros never see it. List them explicitly:
//!     `#[nowui(methods(increment, decrement))]`. Each must exist as
//!     `fn NAME(&mut self, event: &mut nowui_core::Event)` on the type (a
//!     plain `impl Counter { ... }` block, written separately as usual).
//!     `event.node` is a `&mut nowui_core::Node` — the arena node the event
//!     fired on — so a handler can read/mutate its style/kind directly, not
//!     just `self`.
//!   * `#[nowui(view("/path.nowui"))]` bundles a `.nowui` file (and its
//!     *entire* `#`-import graph, transitively) into the binary at compile
//!     time — see `build_embedded_view` below and `nowui_core::NowUiState`'s
//!     `nowui_view`/`nowui_view_path`/`nowui_view_imports` doc comments for
//!     the full mechanics.

use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Ident, Type};

#[proc_macro_derive(NowUiState, attributes(nowui))]
pub fn derive_nowui_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(&input, "#[derive(NowUiState)] requires a struct with named fields")
                    .to_compile_error()
                    .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(&input, "#[derive(NowUiState)] only supports structs")
                .to_compile_error()
                .into();
        }
    };

    let methods = match parse_methods_attr(&input) {
        Ok(m) => m,
        Err(e) => return e.to_compile_error().into(),
    };

    let view = match parse_view_attr(&input) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    let mut get_arms = Vec::new();
    let mut set_arms = Vec::new();
    let mut call_arms = Vec::new();
    let mut object_fields = Vec::new();

    for f in fields {
        let ident = f.ident.as_ref().expect("named field");
        let name_str = ident.to_string();

        if let Some(inner_ty) = vec_inner_type(&f.ty) {
            // `Vec<T>` -> `StateValue::List`, the iterable a `for IDENT in
            // state.path { ... }` loop reads. Read-only for now (no `set`/
            // `call` arm is pushed, so a path landing here falls through to
            // the default `_ => false`) — there's no `.nowui` syntax yet
            // that would write back into a whole list.
            let elem_expr = match scalar_kind(inner_ty) {
                Some(ScalarKind::Str) => quote! { ::nowui_core::StateValue::Str(v.clone()) },
                Some(ScalarKind::Bool) => quote! { ::nowui_core::StateValue::Bool(*v) },
                Some(ScalarKind::Int) => quote! { ::nowui_core::StateValue::Int(*v as i64) },
                Some(ScalarKind::Float) => quote! { ::nowui_core::StateValue::Float(*v as f64) },
                // Not a scalar element type — assume `T: NowUiState` and
                // snapshot each element as a `StateValue::Object` instead,
                // so `${item.field}` can resolve per-iteration.
                None => quote! { ::nowui_core::NowUiState::to_state_value(v) },
            };
            get_arms.push(quote! {
                Some((&#name_str, rest)) if rest.is_empty() => {
                    Some(::nowui_core::StateValue::List(self.#ident.iter().map(|v| #elem_expr).collect()))
                }
            });
            object_fields.push(quote! {
                (#name_str.to_string(), ::nowui_core::StateValue::List(self.#ident.iter().map(|v| #elem_expr).collect()))
            });
            continue;
        }

        match scalar_kind(&f.ty) {
            Some(ScalarKind::Str) => {
                get_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        Some(::nowui_core::StateValue::Str(self.#ident.clone()))
                    }
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        if let Some(v) = value.as_str() { self.#ident = v.to_string(); true } else { false }
                    }
                });
                object_fields.push(quote! { (#name_str.to_string(), ::nowui_core::StateValue::Str(self.#ident.clone())) });
            }
            Some(ScalarKind::Bool) => {
                get_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        Some(::nowui_core::StateValue::Bool(self.#ident))
                    }
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        if let Some(v) = value.as_bool() { self.#ident = v; true } else { false }
                    }
                });
                object_fields.push(quote! { (#name_str.to_string(), ::nowui_core::StateValue::Bool(self.#ident)) });
            }
            Some(ScalarKind::Int) => {
                let ty = &f.ty;
                get_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        Some(::nowui_core::StateValue::Int(self.#ident as i64))
                    }
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        if let Some(v) = value.as_i64() { self.#ident = v as #ty; true } else { false }
                    }
                });
                object_fields.push(quote! { (#name_str.to_string(), ::nowui_core::StateValue::Int(self.#ident as i64)) });
            }
            Some(ScalarKind::Float) => {
                let ty = &f.ty;
                get_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        Some(::nowui_core::StateValue::Float(self.#ident as f64))
                    }
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        if let Some(v) = value.as_f64() { self.#ident = v as #ty; true } else { false }
                    }
                });
                object_fields.push(quote! { (#name_str.to_string(), ::nowui_core::StateValue::Float(self.#ident as f64)) });
            }
            None => {
                // Not a recognized leaf type — assume it's a nested
                // `NowUiState` and delegate, at any remaining depth. `call`
                // delegates too (not just `get`/`set`) — a method declared
                // via `#[nowui(methods(...))]` on a *nested* struct (e.g.
                // `counter: Counter`, with `Counter`'s own `increment`) is
                // only reachable this way, since a derive on the outer
                // struct never sees the inner one's method list.
                get_arms.push(quote! {
                    Some((&#name_str, rest)) => ::nowui_core::NowUiState::get(&self.#ident, rest),
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) => ::nowui_core::NowUiState::set(&mut self.#ident, rest, value),
                });
                call_arms.push(quote! {
                    Some((&#name_str, rest)) => ::nowui_core::NowUiState::call(&mut self.#ident, rest, event),
                });
                object_fields.push(quote! {
                    (#name_str.to_string(), ::nowui_core::NowUiState::to_state_value(&self.#ident))
                });
            }
        }
    }

    call_arms.extend(methods.iter().map(|m| {
        let name_str = m.to_string();
        quote! {
            Some((&#name_str, rest)) if rest.is_empty() => {
                self.#m(event);
                true
            }
        }
    }));

    // `#[nowui(view("/login.nowui"))]` embeds the entry file's contents
    // *and* its whole `#`-import graph into the binary at compile time —
    // see `build_embedded_view`. No attribute at all: the trait defaults
    // (`None`) apply, unchanged.
    let nowui_view_fn = match view {
        Some(rel) => match build_embedded_view(&rel) {
            Ok(fns) => fns,
            Err(e) => return e.to_compile_error().into(),
        },
        None => quote! {},
    };

    let expanded = quote! {
        impl ::nowui_core::NowUiState for #name {
            fn get(&self, path: &[&str]) -> Option<::nowui_core::StateValue> {
                match path.split_first() {
                    #(#get_arms)*
                    _ => None,
                }
            }

            fn set(&mut self, path: &[&str], value: ::nowui_core::StateValue) -> bool {
                match path.split_first() {
                    #(#set_arms)*
                    _ => false,
                }
            }

            fn call(&mut self, path: &[&str], event: &mut ::nowui_core::Event<'_>) -> bool {
                match path.split_first() {
                    #(#call_arms)*
                    _ => false,
                }
            }

            fn to_state_value(&self) -> ::nowui_core::StateValue {
                ::nowui_core::StateValue::Object(vec![ #(#object_fields),* ])
            }

            #nowui_view_fn
        }
    };

    expanded.into()
}

enum ScalarKind {
    Str,
    Bool,
    Int,
    Float,
}

/// Classify a field's type by its bare (unqualified) name — `String`,
/// `bool`, or a numeric primitive become leaf `StateValue`s (integer types
/// as `StateValue::Int`, `f32`/`f64` as `StateValue::Float` — kept distinct
/// so display code doesn't have to guess a field's original type back from
/// a collapsed `f64`); anything else (including `std::string::String`
/// written out in full — a known limitation, see the module docs) is
/// treated as a nested `NowUiState`.
fn scalar_kind(ty: &Type) -> Option<ScalarKind> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    match seg.ident.to_string().as_str() {
        "String" => Some(ScalarKind::Str),
        "bool" => Some(ScalarKind::Bool),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => {
            Some(ScalarKind::Int)
        }
        "f32" | "f64" => Some(ScalarKind::Float),
        _ => None,
    }
}

/// If `ty` is `Vec<T>`, return `T` — regardless of whether `T` is a leaf
/// scalar (the caller decides that separately via `scalar_kind`).
fn vec_inner_type(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else { return None };
    let syn::GenericArgument::Type(inner_ty) = args.args.first()? else { return None };
    Some(inner_ty)
}

/// Parse `#[nowui(methods(a, b, c))]` into `[a, b, c]`. No attribute at all
/// is fine (no callable methods, just field reflection). Ignores `view(...)`
/// (see `parse_view_attr`) rather than erroring on it — both can appear in
/// the same `#[nowui(...)]` list, or in separate `#[nowui(...)]` attributes.
fn parse_methods_attr(input: &DeriveInput) -> syn::Result<Vec<Ident>> {
    let mut methods = Vec::new();
    for attr in &input.attrs {
        if !attr.path().is_ident("nowui") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("methods") {
                meta.parse_nested_meta(|inner| {
                    if let Some(ident) = inner.path.get_ident() {
                        methods.push(ident.clone());
                        Ok(())
                    } else {
                        Err(inner.error("expected a method name"))
                    }
                })
            } else if meta.path.is_ident("view") {
                // Handled by `parse_view_attr`; consume the `("...")` here
                // too so `parse_nested_meta` doesn't error on it.
                let content;
                syn::parenthesized!(content in meta.input);
                let _: syn::LitStr = content.parse()?;
                Ok(())
            } else {
                Err(meta.error("unknown `nowui` attribute — expected `methods(...)` or `view(...)`"))
            }
        })?;
    }
    Ok(methods)
}

/// Parse `#[nowui(view("/login.nowui"))]` into `Some("/login.nowui")` — the
/// path (relative to this crate's own `src/` directory) of the `.nowui` file
/// to embed into the binary at compile time via `include_str!`. No `view`
/// attribute at all is fine (`None` — the type isn't backed by a bundled
/// view; `nowui_runtime::run_path` loads one from disk at runtime instead).
fn parse_view_attr(input: &DeriveInput) -> syn::Result<Option<syn::LitStr>> {
    let mut view = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("nowui") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("view") {
                let content;
                syn::parenthesized!(content in meta.input);
                let lit: syn::LitStr = content.parse()?;
                view = Some(lit);
                Ok(())
            } else if meta.path.is_ident("methods") {
                // Handled by `parse_methods_attr`; consume `(a, b, c)` here
                // too so `parse_nested_meta` doesn't error on it.
                meta.parse_nested_meta(|_inner| Ok(()))
            } else {
                Err(meta.error("unknown `nowui` attribute — expected `methods(...)` or `view(...)`"))
            }
        })?;
    }
    Ok(view)
}

/// Walk the whole `#`-import graph starting at `rel` (relative to this
/// crate's own `src/` directory — `CARGO_MANIFEST_DIR` is read from the
/// proc-macro's own process environment, which is the *consuming* crate's
/// manifest dir, since the macro executes as part of compiling that crate),
/// reading and parsing every file it transitively imports, and generate the
/// `nowui_view`/`nowui_view_path`/`nowui_view_imports` trait method bodies.
///
/// Every file's actual bytes are embedded via `include_str!` on its absolute
/// path (not by splicing the string we read here into the generated code) —
/// `include_str!` gives rustc proper compile-time dependency tracking (the
/// crate rebuilds if any embedded `.nowui` file changes), which a bare
/// string literal wouldn't. We still need to read each file ourselves,
/// separately, purely to *parse* it and discover its own further imports —
/// a small double-read at compile time only, no runtime cost.
fn build_embedded_view(rel: &syn::LitStr) -> syn::Result<proc_macro2::TokenStream> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new_spanned(rel, "CARGO_MANIFEST_DIR not set — can't resolve #[nowui(view(...))]"))?;
    let src_dir = PathBuf::from(manifest_dir).join("src");

    let rel_str = rel.value();
    let entry_key = rel_str.trim_start_matches('/').replace('\\', "/");
    let entry_abs = src_dir.join(&entry_key);
    let entry_content = std::fs::read_to_string(&entry_abs).map_err(|e| {
        syn::Error::new_spanned(rel, format!("could not read bundled view `{}` ({}): {e}", rel_str, entry_abs.display()))
    })?;
    let entry_dir = nowui_syntax::import_dirname(&entry_key).to_string();

    let mut imports: Vec<(String, PathBuf)> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(entry_key.clone());
    walk_imports(rel, &entry_content, &entry_dir, &src_dir, &mut visited, &mut imports)?;

    let entry_abs_str = entry_abs.to_string_lossy().into_owned();
    let import_arms = imports.iter().map(|(key, abs)| {
        let abs_str = abs.to_string_lossy().into_owned();
        quote! { (#key, include_str!(#abs_str)) }
    });

    Ok(quote! {
        fn nowui_view() -> Option<&'static str> where Self: Sized {
            Some(include_str!(#entry_abs_str))
        }
        fn nowui_view_path() -> Option<&'static str> where Self: Sized {
            Some(#rel_str)
        }
        fn nowui_view_imports() -> Option<&'static [(&'static str, &'static str)]> where Self: Sized {
            Some(&[ #(#import_arms),* ])
        }
    })
}

/// Parse `source` (the contents of the file at `dir`'s own level), find
/// every `#`-import it declares, and recurse into each one — depth-first,
/// so a diamond import (two files importing the same third file) or an
/// import cycle is caught by `visited` (keyed by the normalized,
/// `nowui_syntax::join_import_path`-computed path) exactly the same way
/// `nowui-runtime`'s on-disk loader dedupes/breaks cycles, just lexically
/// instead of via `Path::canonicalize` (there's no requirement that the
/// files really exist relative to each other in some canonical sense — only
/// that the same key resolves the same way here and in the runtime loader).
fn walk_imports(
    view_attr: &syn::LitStr,
    source: &str,
    dir: &str,
    src_dir: &Path,
    visited: &mut HashSet<String>,
    out: &mut Vec<(String, PathBuf)>,
) -> syn::Result<()> {
    let ast = nowui_syntax::parse(source)
        .map_err(|errors| syn::Error::new_spanned(view_attr, format!("parse error(s) in bundled view graph: {errors:?}")))?;

    for node in ast {
        if let nowui_syntax::ast::Node::Import { path: rel } = node {
            let key = nowui_syntax::join_import_path(dir, &rel);
            if !visited.insert(key.clone()) {
                continue; // diamond import already embedded, or an import cycle
            }
            let abs = src_dir.join(&key);
            let content = std::fs::read_to_string(&abs).map_err(|e| {
                syn::Error::new_spanned(view_attr, format!("could not read bundled import `{rel}` ({}): {e}", abs.display()))
            })?;
            let child_dir = nowui_syntax::import_dirname(&key).to_string();
            walk_imports(view_attr, &content, &child_dir, src_dir, visited, out)?;
            out.push((key, abs));
        }
    }
    Ok(())
}
