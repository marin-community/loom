//! Derives that project one operation declaration onto REST, CLI, and MCP.
//!
//! An operation is declared once — `#[operation(...)]` on a unit struct, plus an
//! `Input` type carrying `#[derive(Operands)]`. Everything a transport needs is
//! read back off that declaration: the JSON body, the MCP argument schema, the
//! clap flags, and the authority metadata. There is deliberately no way to
//! describe an operation without also defining it.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

mod field;
mod operation;

/// Derive the caller-facing argument surface of an operation.
///
/// Emits one impl carrying the JSON Schema (context fields elided), the clap
/// projection, and the dispatcher hooks that fill context fields. The type's
/// serde impls are left alone: this derive describes arguments, it does not
/// reinvent serialization.
#[proc_macro_derive(Operands, attributes(operand))]
pub fn derive_operands(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_operands(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive a presentation-only flag set.
///
/// View flags never cross the wire. They exist so `--mine` and `--repo` can go
/// on keeping their CLI affordance without being mistaken for operation
/// arguments, which is what let the old registry's `args` drift from `Input`.
#[proc_macro_derive(View, attributes(operand))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_view(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare one operation: its identity, authority, and transport projections.
#[proc_macro_attribute]
pub fn operation(args: TokenStream, item: TokenStream) -> TokenStream {
    operation::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn named_fields(input: &DeriveInput) -> syn::Result<Vec<field::Operand>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "operand derives apply to structs",
        ));
    };
    let fields = match &data.fields {
        Fields::Named(named) => named.named.iter().collect::<Vec<_>>(),
        Fields::Unit => Vec::new(),
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "operand structs need named fields",
            ))
        }
    };
    fields.into_iter().map(field::parse).collect()
}

fn expand_operands(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let operands = named_fields(&input)?;

    for operand in &operands {
        if operand.kind == field::Kind::Json
            && operand.context.is_none()
            && !operand.skip_cli
            && operand.positional
        {
            return Err(syn::Error::new(
                operand.ident.span(),
                "a JSON-shaped operand cannot be positional; give it a --flag",
            ));
        }
    }

    let args = operands
        .iter()
        .filter(|operand| operand.is_caller_supplied())
        .collect::<Vec<_>>();

    let clap_args = args.iter().copied().map(field::clap_arg);
    let builders = operands.iter().map(field::from_matches);

    // Context fields are stripped from the derived schema rather than skipped at
    // the serde layer: the field still travels on the wire, it just is not
    // something a caller may supply.
    let context_names = operands
        .iter()
        .filter(|operand| operand.context.is_some())
        .map(|operand| operand.name.clone())
        .collect::<Vec<_>>();

    let context_specs = operands.iter().filter_map(|operand| {
        let source = operand.context?.tokens();
        let name = &operand.name;
        Some(quote! {
            ::weaver_api::operations::ContextField { name: #name, source: #source }
        })
    });
    let context_fills = operands.iter().map(source_setter);

    // The wire counterpart of clap's defaults, emitted from the very same
    // expression so a value a caller may omit means the same thing on the
    // command line and over HTTP.
    let wire_defaults = operands.iter().filter_map(|operand| {
        let name = &operand.name;
        let value = if let Some(default) = &operand.default {
            {
                let ty = &operand.ty;
                quote!(::serde_json::to_value::<#ty>(::core::convert::Into::into(#default))
                    .unwrap_or(::serde_json::Value::Null))
            }
        } else if operand.kind.is_optional() {
            quote!(::serde_json::Value::Null)
        } else if operand.kind.is_multi() {
            quote!(::serde_json::Value::Array(::std::vec::Vec::new()))
        } else {
            return None;
        };
        Some(quote!(object.insert(::std::string::String::from(#name), #value);))
    });

    Ok(quote! {
        impl ::weaver_api::operations::Operands for #name {
            const CONTEXT: &'static [::weaver_api::operations::ContextField] =
                &[#(#context_specs),*];

            fn schema() -> ::serde_json::Value {
                let mut schema = ::serde_json::to_value(
                    ::schemars::schema_for!(#name)
                )
                .unwrap_or_else(|_| ::serde_json::json!({ "type": "object" }));
                ::weaver_api::operations::strip_context_fields(
                    &mut schema,
                    &[#(#context_names),*],
                );
                schema
            }

            fn augment(mut cmd: ::clap::Command) -> ::clap::Command {
                #(#clap_args)*
                cmd
            }

            fn from_matches(matches: &::clap::ArgMatches) -> ::core::result::Result<Self, String> {
                Ok(Self { #(#builders)* })
            }

            fn wire_defaults() -> ::serde_json::Value {
                let mut object = ::serde_json::Map::new();
                #(#wire_defaults)*
                ::serde_json::Value::Object(object)
            }

            fn fill_context(
                &mut self,
                context: &::weaver_api::operations::ContextValues,
            ) {
                #(#context_fills)*
            }
        }
    })
}

fn source_setter(operand: &field::Operand) -> proc_macro2::TokenStream {
    let ident = &operand.ident;
    let Some(source) = operand.context else {
        return quote!();
    };
    let getter = match source {
        field::ContextSource::RepoRoot => quote!(context.repo_root.clone()),
        field::ContextSource::Branch => quote!(context.branch.clone()),
        field::ContextSource::BranchName => quote!(context.branch_name.clone()),
        field::ContextSource::Session => quote!(context.session.clone()),
    };
    // Only fill what the caller left empty, so an explicitly supplied override
    // (a `--branch` on a session-scoped command) still wins.
    quote! {
        if ::weaver_api::operations::is_unset(&self.#ident) {
            self.#ident = ::core::convert::From::from(#getter);
        }
    }
}

fn expand_view(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let operands = named_fields(&input)?;

    for operand in &operands {
        if operand.context.is_some() {
            return Err(syn::Error::new(
                operand.ident.span(),
                "view flags are presentation-only and cannot take context values",
            ));
        }
    }

    let clap_args = operands.iter().map(field::clap_arg);
    let builders = operands.iter().map(field::from_matches);

    Ok(quote! {
        impl ::weaver_api::operations::ViewFlags for #name {
            fn augment(mut cmd: ::clap::Command) -> ::clap::Command {
                #(#clap_args)*
                cmd
            }

            fn from_matches(matches: &::clap::ArgMatches) -> ::core::result::Result<Self, String> {
                Ok(Self { #(#builders)* })
            }
        }
    })
}
