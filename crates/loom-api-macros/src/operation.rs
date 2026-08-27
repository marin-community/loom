//! The `#[operation(...)]` declaration.
//!
//! One attribute on the operation's `Input` struct produces everything else:
//! the serde/schema/clap derives the transports read, the `Op` marker they are
//! reached through, and the descriptor carrying identity and authority. An
//! operation is its arguments plus a policy, and this is the one place both are
//! written down.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, ExprArray, ExprLit, Ident, ItemStruct, Lit, LitStr, Meta, Token,
};

use crate::field::{doc_comment, outer_ident};

struct Args {
    entries: Punctuated<Entry, Token![,]>,
}

struct Entry {
    key: Ident,
    value: Expr,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            entries: Punctuated::parse_terminated(input)?,
        })
    }
}

impl Parse for Entry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let value: Expr = input.parse()?;
        Ok(Self { key, value })
    }
}

fn as_string(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => Ok(text.value()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

fn as_ident(expr: &Expr) -> syn::Result<Ident> {
    match expr {
        Expr::Path(path) => path
            .path
            .get_ident()
            .cloned()
            .ok_or_else(|| syn::Error::new_spanned(path, "expected a bare identifier")),
        other => Err(syn::Error::new_spanned(other, "expected a bare identifier")),
    }
}

fn as_string_list(expr: &Expr) -> syn::Result<Vec<String>> {
    match expr {
        Expr::Array(ExprArray { elems, .. }) => elems.iter().map(as_string).collect(),
        other => Err(syn::Error::new_spanned(
            other,
            "expected an array of string literals",
        )),
    }
}

pub fn expand(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: Args = syn::parse2(args)?;
    let mut item: ItemStruct = syn::parse2(item)?;
    let input_ty = item.ident.clone();
    let span = input_ty.span();

    let mut id = None;
    let mut actor = None;
    let mut scope = None;
    let mut risk = None;
    let mut io = None;
    let mut grants = Vec::new();
    let mut cli = None;
    let mut cli_aliases = Vec::new();
    let mut view_ty = None;
    let mut custom_render = false;
    let mut custom_scoped = false;
    let mut custom_default = false;

    for entry in &args.entries {
        match entry.key.to_string().as_str() {
            "id" => id = Some(as_string(&entry.value)?),
            "actor" => actor = Some(as_ident(&entry.value)?),
            "scope" => scope = Some(as_ident(&entry.value)?),
            "risk" => risk = Some(as_ident(&entry.value)?),
            "io" => io = Some(as_ident(&entry.value)?),
            "grants" => grants = as_string_list(&entry.value)?,
            "cli" => cli = Some(as_string(&entry.value)?),
            "cli_alias" => cli_aliases.push(as_string(&entry.value)?),
            "view" => view_ty = Some(as_ident(&entry.value)?),
            "render" => custom_render = as_ident(&entry.value)? == "custom",
            "scoped" => custom_scoped = as_ident(&entry.value)? == "custom",
            "default" => custom_default = as_ident(&entry.value)? == "custom",
            other => {
                return Err(syn::Error::new_spanned(
                    &entry.key,
                    format!("unknown operation attribute `{other}`"),
                ))
            }
        }
    }

    let id = id.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs an `id`"))?;
    // The bundle is the id's first dotted segment. One less thing to keep in
    // agreement with the module path.
    let bundle = id
        .split('.')
        .next()
        .unwrap_or_default()
        .replace('_', "-")
        .to_string();
    // The doc comment is the summary. Help text and the declaration cannot drift
    // apart if there is only one of them.
    let summary = doc_comment(&item.attrs).ok_or_else(|| {
        syn::Error::new_spanned(&item, "operation needs a doc comment to use as its summary")
    })?;

    let actor =
        actor.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs an `actor`"))?;
    let scope = scope.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs a `scope`"))?;
    let risk = risk.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs a `risk`"))?;
    let io = io.unwrap_or_else(|| Ident::new("Json", span));

    let view_path = match &view_ty {
        Some(view) => quote!(#view),
        None => quote!(::weaver_api::operations::NoView),
    };

    let cli_tokens = match &cli {
        Some(path) => {
            let segments = path.split_whitespace().collect::<Vec<_>>();
            if segments.is_empty() {
                return Err(syn::Error::new_spanned(
                    &item,
                    "`cli` must name at least one command segment",
                ));
            }
            let segments = segments.iter().map(|segment| LitStr::new(segment, span));
            let aliases = cli_aliases.iter().map(|alias| LitStr::new(alias, span));
            quote!(Some(::weaver_api::operations::CliProjection {
                path: &[#(#segments),*],
                aliases: &[#(#aliases),*],
            }))
        }
        None => {
            if !cli_aliases.is_empty() {
                return Err(syn::Error::new_spanned(
                    &item,
                    "`cli_alias` without `cli` has nothing to alias",
                ));
            }
            quote!(None)
        }
    };

    let grant_literals = grants.iter().map(|grant| LitStr::new(grant, span));

    // `Scoped` for free: every operation's resource check reduces to "which
    // context field names the resource", and that's exactly what `scope`
    // already says. `scoped = custom` opts out for the rare operation whose
    // resource isn't one of its own context fields.
    let scope_ref = match scope.to_string().as_str() {
        _ if custom_scoped => quote!(),
        "Global" => quote!(::weaver_api::operations::ScopeRef::Global),
        "Session" => quote!(::weaver_api::operations::ScopeRef::Session(&self.session)),
        "Branch" => quote!(::weaver_api::operations::ScopeRef::Branch(&self.branch)),
        "Channel" => quote!(::weaver_api::operations::ScopeRef::Channel(&self.channel)),
        "Repository" => quote!(::weaver_api::operations::ScopeRef::Repository(
            &self.repo_root
        )),
        other => {
            return Err(syn::Error::new_spanned(
                &item,
                format!(
                    "no default `Scoped` impl for `scope = {other}`; add `scoped = custom` \
                     and implement it by hand"
                ),
            ))
        }
    };
    let scoped_impl = if custom_scoped {
        quote!()
    } else {
        quote! {
            impl ::weaver_api::operations::Scoped for #input_ty {
                fn scope_ref(&self) -> ::weaver_api::operations::ScopeRef<'_> {
                    #scope_ref
                }
            }
        }
    };

    // Every operation is reached through `Op`, so a caller writes
    // `ops::branches::get::Op` and never has to learn a per-operation name.
    let marker = Ident::new("Op", span);
    let marker_doc = format!("The `{id}` operation.");
    // `Default` completes the fields an MCP caller omitted (see the MCP
    // dispatcher). `default = custom` opts out where a field's type has no
    // sensible default of its own and the impl is written by hand.
    let default_derive = if custom_default {
        quote!()
    } else {
        quote!(Default,)
    };

    // A JSON renderer for free. `render = custom` opts out so a bundle can
    // write a real one without a conflicting impl.
    let render_impl = if custom_render {
        quote!()
    } else {
        quote!(impl ::weaver_api::operations::Render for #marker {})
    };

    let defaults = serde_defaults(&mut item)?;

    let attrs = std::mem::take(&mut item.attrs);
    let vis = item.vis.clone();

    Ok(quote! {
        #(#attrs)*
        #[derive(
            Debug,
            Clone,
            #default_derive
            ::serde::Serialize,
            ::serde::Deserialize,
            ::schemars::JsonSchema,
            ::loom_api_macros::Operands,
        )]
        #item

        #(#defaults)*

        #[doc = #marker_doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #vis struct #marker;

        impl ::weaver_api::operations::Operation for #marker {
            type Input = #input_ty;
            type Output = Output;
            type View = #view_path;

            const SPEC: &'static ::weaver_api::operations::OperationSpec =
                &::weaver_api::operations::OperationSpec {
                    id: #id,
                    bundle: #bundle,
                    summary: #summary,
                    actor: ::weaver_api::operations::ActorPolicy::#actor,
                    scope: ::weaver_api::operations::OperationScope::#scope,
                    risk: ::weaver_api::operations::OperationRisk::#risk,
                    io: ::weaver_api::operations::Io::#io,
                    grants: &[#(#grant_literals),*],
                    cli: #cli_tokens,
                    schema: <#input_ty as ::weaver_api::operations::Operands>::schema,
                    output_schema: || {
                        ::serde_json::to_value(::schemars::schema_for!(Output))
                            .unwrap_or_else(|_| ::serde_json::json!({}))
                    },
                    context: <#input_ty as ::weaver_api::operations::Operands>::CONTEXT,
                };
        }

        #[doc = #marker_doc]
        #vis const SPEC: &'static ::weaver_api::operations::OperationSpec =
            <#marker as ::weaver_api::operations::Operation>::SPEC;

        #render_impl

        #scoped_impl
    })
}

/// Give every field a caller may omit the `serde` default it already declares.
///
/// `#[operand(default = ...)]` used to be honoured only by the dispatchers,
/// which merged the declared values into the JSON before handing it to `serde`.
/// That left two things wrong. `schemars` reads `serde` attributes, so a field
/// with a declared default was still advertised as `required` — an MCP tool
/// demanded arguments the operation had said were optional. And an `Input`
/// nested inside another operation's `Input` never reached a dispatcher at all,
/// so it could only be deserialized fully spelled out.
///
/// Declaring the default where `serde` can see it fixes both, and the schema
/// gains the `default` keyword for free. The dispatchers no longer merge
/// anything.
fn serde_defaults(item: &mut ItemStruct) -> syn::Result<Vec<TokenStream>> {
    let struct_ty = item.ident.clone();
    let mut functions = Vec::new();
    for field in item.fields.iter_mut() {
        let operand = crate::field::parse(field)?;
        let Some(default) = operand.default else {
            // A context field is never sent by a caller, and an absent
            // `Option`, `Vec`, or `bool` has an obvious meaning. Anything else
            // stays required.
            if !has_serde_default(&field.attrs)
                && (operand.context.is_some()
                    || matches!(
                        outer_ident(&field.ty).as_deref(),
                        Some("Option" | "Vec" | "bool")
                    ))
            {
                field.attrs.push(syn::parse_quote!(#[serde(default)]));
            }
            continue;
        };
        // A hand-written `#[serde(default)]` on a field that also declares one
        // would resolve to `Default::default()` and quietly disagree with the
        // declaration. The declaration wins; anything else in the same
        // attribute (an `alias`, say) is kept.
        strip_serde_default(&mut field.attrs);
        let ty = &field.ty;
        let name = &operand.ident;
        let function = Ident::new(
            &format!("__default_{struct_ty}_{name}"),
            proc_macro2::Span::call_site(),
        );
        let path = function.to_string();
        field
            .attrs
            .push(syn::parse_quote!(#[serde(default = #path)]));
        functions.push(quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            fn #function() -> #ty {
                ::core::convert::Into::into(#default)
            }
        });
    }
    Ok(functions)
}

/// Whether `serde` already has a default for this field.
fn has_serde_default(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("serde")
            && serde_entries(attr).is_some_and(|entries| entries.iter().any(is_default_entry))
    })
}

/// Drop `default` from every `#[serde(...)]` on this field, keeping the rest.
fn strip_serde_default(attrs: &mut Vec<syn::Attribute>) {
    attrs.retain_mut(|attr| {
        if !attr.path().is_ident("serde") {
            return true;
        }
        let Some(entries) = serde_entries(attr) else {
            return true;
        };
        let kept = entries
            .into_iter()
            .filter(|entry| !is_default_entry(entry))
            .collect::<Punctuated<Meta, Token![,]>>();
        if kept.is_empty() {
            return false;
        }
        *attr = syn::parse_quote!(#[serde(#kept)]);
        true
    });
}

fn serde_entries(attr: &syn::Attribute) -> Option<Vec<Meta>> {
    attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()
        .map(|entries| entries.into_iter().collect())
}

fn is_default_entry(entry: &Meta) -> bool {
    entry.path().is_ident("default")
}
