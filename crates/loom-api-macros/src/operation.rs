//! The `#[operation(...)]` declaration.
//!
//! One attribute on a unit struct produces the descriptor *and* binds it to the
//! `Input`/`Output` types beside it. Because the descriptor is emitted from the
//! same item that names the types, a declaration cannot describe an operation
//! that does not exist, and an operation cannot exist without a declaration.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, ExprArray, ExprLit, Ident, ItemStruct, Lit, LitStr, Token,
};

use crate::field::doc_comment;

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
    let item: ItemStruct = syn::parse2(item)?;
    let name = &item.ident;

    let mut id = None;
    let mut bundle = None;
    let mut summary = None;
    let mut actor = None;
    let mut scope = None;
    let mut risk = None;
    let mut io = None;
    let mut grants = Vec::new();
    let mut cli = None;
    let mut cli_aliases = Vec::new();
    let mut mcp = None;
    let mut input_ty = None;
    let mut output_ty = None;
    let mut view_ty = None;
    let mut custom_render = false;

    for entry in &args.entries {
        match entry.key.to_string().as_str() {
            "id" => id = Some(as_string(&entry.value)?),
            "bundle" => bundle = Some(as_string(&entry.value)?),
            "summary" => summary = Some(as_string(&entry.value)?),
            "actor" => actor = Some(as_ident(&entry.value)?),
            "scope" => scope = Some(as_ident(&entry.value)?),
            "risk" => risk = Some(as_ident(&entry.value)?),
            "io" => io = Some(as_ident(&entry.value)?),
            "grants" => grants = as_string_list(&entry.value)?,
            "cli" => cli = Some(as_string(&entry.value)?),
            "cli_alias" => cli_aliases.push(as_string(&entry.value)?),
            "cli_aliases" => cli_aliases = as_string_list(&entry.value)?,
            "mcp" => mcp = Some(as_string(&entry.value)?),
            "input" => input_ty = Some(as_ident(&entry.value)?),
            "output" => output_ty = Some(as_ident(&entry.value)?),
            "view" => view_ty = Some(as_ident(&entry.value)?),
            "render" => custom_render = as_ident(&entry.value)? == "custom",
            other => {
                return Err(syn::Error::new_spanned(
                    &entry.key,
                    format!("unknown operation attribute `{other}`"),
                ))
            }
        }
    }

    let id = id.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs an `id`"))?;
    // The bundle is the id's first dotted segment unless stated. One less thing
    // to keep in agreement with the module path.
    let bundle = bundle.unwrap_or_else(|| {
        id.split('.')
            .next()
            .unwrap_or_default()
            .replace('_', "-")
            .to_string()
    });
    // The doc comment is the summary. Help text and the declaration cannot drift
    // apart if there is only one of them.
    let summary = summary
        .or_else(|| doc_comment(&item.attrs))
        .ok_or_else(|| {
            syn::Error::new_spanned(
                &item,
                "operation needs a doc comment or an explicit `summary`",
            )
        })?;

    let actor =
        actor.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs an `actor`"))?;
    let scope = scope.unwrap_or_else(|| Ident::new("Session", name.span()));
    let risk = risk.ok_or_else(|| syn::Error::new_spanned(&item, "operation needs a `risk`"))?;
    let io = io.unwrap_or_else(|| Ident::new("Json", name.span()));

    let input_ty = input_ty.unwrap_or_else(|| Ident::new("Input", name.span()));
    let output_ty = output_ty.unwrap_or_else(|| Ident::new("Output", name.span()));
    let view_ty = view_ty.unwrap_or_else(|| Ident::new("NoView", name.span()));
    let view_path = if view_ty == "NoView" {
        quote!(::weaver_api::operations::NoView)
    } else {
        quote!(#view_ty)
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
            let segments = segments
                .iter()
                .map(|segment| LitStr::new(segment, name.span()));
            let aliases = cli_aliases
                .iter()
                .map(|alias| LitStr::new(alias, name.span()));
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

    let mcp_tokens = match &mcp {
        Some(projection) => {
            let (server, tool) = projection.split_once("::").ok_or_else(|| {
                syn::Error::new_spanned(&item, "`mcp` looks like \"server::tool\"")
            })?;
            let server = LitStr::new(server, name.span());
            let tool = LitStr::new(tool, name.span());
            quote!(Some(::weaver_api::operations::McpProjection {
                server: #server,
                tool: #tool,
            }))
        }
        None => quote!(None),
    };

    let grant_literals = grants.iter().map(|grant| LitStr::new(grant, name.span()));

    let attrs = &item.attrs;
    let vis = &item.vis;

    // A JSON renderer for free. `render = custom` opts out so a bundle can
    // write a real one without a conflicting impl.
    let render_impl = if custom_render {
        quote!()
    } else {
        quote! {
            impl ::weaver_api::operations::Render for #name {}
        }
    };

    Ok(quote! {
        #(#attrs)*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #vis struct #name;

        impl ::weaver_api::operations::Operation for #name {
            type Input = #input_ty;
            type Output = #output_ty;
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
                    mcp: #mcp_tokens,
                    schema: <#input_ty as ::weaver_api::operations::Operands>::schema,
                    context: <#input_ty as ::weaver_api::operations::Operands>::CONTEXT,
                };
        }

        #render_impl
    })
}
