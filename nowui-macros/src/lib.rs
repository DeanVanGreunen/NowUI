//! `#[derive(NowUiState)]` — generates the string-path reflection glue
//! (`nowui_core::NowUiState::get`/`set`/`call`) a live app-state struct needs
//! to back `.nowui` `{value: state.foo.bar}` bindings and `{onClick:
//! state.foo.bar}` callbacks. See `nowui-core/src/state.rs` for the trait
//! itself and `CLAUDE.md` for the end-to-end reactivity design.
//!
//! Scope (deliberately small — extend the match statements below if you
//! need more):
//!   * Named-field structs only (no tuples/units/enums).
//!   * Leaf field types: `String`, `bool`, and any integer/float primitive
//!     (normalized to `StateValue::Number` as `f64`). Any other field type is
//!     assumed to itself implement `NowUiState` and gets a *delegating*
//!     get/set/call arm (e.g. `counter: Counter` where `Counter` also
//!     derives `NowUiState`) — this is a syntactic guess (derive macros
//!     can't see trait bounds), so a wrongly-typed field just fails to
//!     compile with a normal trait-not-implemented error.
//!   * Callable methods aren't discovered from the struct's `impl` block —
//!     derive macros never see it. List them explicitly:
//!     `#[nowui(methods(increment, decrement))]`. Each must exist as
//!     `fn NAME(&mut self, event: &nowui_core::Event)` on the type (a plain
//!     `impl Counter { ... }` block, written separately as usual).

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

    let mut get_arms = Vec::new();
    let mut set_arms = Vec::new();
    let mut call_arms = Vec::new();

    for f in fields {
        let ident = f.ident.as_ref().expect("named field");
        let name_str = ident.to_string();

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
            }
            Some(ScalarKind::Number) => {
                let ty = &f.ty;
                get_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        Some(::nowui_core::StateValue::Number(self.#ident as f64))
                    }
                });
                set_arms.push(quote! {
                    Some((&#name_str, rest)) if rest.is_empty() => {
                        if let Some(v) = value.as_f64() { self.#ident = v as #ty; true } else { false }
                    }
                });
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

            fn call(&mut self, path: &[&str], event: &::nowui_core::Event) -> bool {
                match path.split_first() {
                    #(#call_arms)*
                    _ => false,
                }
            }
        }
    };

    expanded.into()
}

enum ScalarKind {
    Str,
    Bool,
    Number,
}

/// Classify a field's type by its bare (unqualified) name — `String`,
/// `bool`, or a numeric primitive become leaf `StateValue`s; anything else
/// (including `std::string::String` written out in full — a known
/// limitation, see the module docs) is treated as a nested `NowUiState`.
fn scalar_kind(ty: &Type) -> Option<ScalarKind> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    match seg.ident.to_string().as_str() {
        "String" => Some(ScalarKind::Str),
        "bool" => Some(ScalarKind::Bool),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" | "f32" | "f64" => Some(ScalarKind::Number),
        _ => None,
    }
}

/// Parse `#[nowui(methods(a, b, c))]` into `[a, b, c]`. No attribute at all
/// is fine (no callable methods, just field reflection).
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
            } else {
                Err(meta.error("unknown `nowui` attribute — expected `methods(...)`"))
            }
        })?;
    }
    Ok(methods)
}
