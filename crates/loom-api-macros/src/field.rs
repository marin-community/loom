//! Field-level parsing shared by the `Operands` and `View` derives.
//!
//! Both derives project the *same* struct onto a clap surface, so the attribute
//! vocabulary and the type classification live here rather than being written
//! twice with a chance to disagree.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Expr, Field, Ident, Lit, LitStr, Meta, Type};

/// How a field's Rust type projects onto a command line.
///
/// Anything the CLI cannot express as a flag or positional lands in `Json`,
/// which is deliberately explicit: an operand whose shape is a nested struct or
/// a tagged union has to say so, rather than silently degrading to a string.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Str,
    OptInt,
    OptStr,
    OptBool,
    VecStr,
    VecInt,
    Json,
}

impl Kind {
    pub fn is_optional(self) -> bool {
        matches!(self, Kind::OptInt | Kind::OptStr | Kind::OptBool | Kind::Bool)
    }

    pub fn is_multi(self) -> bool {
        matches!(self, Kind::VecStr | Kind::VecInt)
    }
}

/// Which value the dispatcher fills a `#[operand(context)]` field from.
///
/// Inferred from the field name so the ordinary case needs no argument; the
/// explicit form exists for fields whose name does not match the context key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContextSource {
    RepoRoot,
    Branch,
    BranchName,
    Session,
}

impl ContextSource {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "repo_root" => Some(Self::RepoRoot),
            "branch" | "branch_id" => Some(Self::Branch),
            "branch_name" | "source_branch" => Some(Self::BranchName),
            "session" | "session_id" => Some(Self::Session),
            _ => None,
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "repo_root" => Some(Self::RepoRoot),
            "branch" => Some(Self::Branch),
            "branch_name" => Some(Self::BranchName),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    pub fn tokens(self) -> TokenStream {
        match self {
            Self::RepoRoot => quote!(::weaver_api::operations::ContextSource::RepoRoot),
            Self::Branch => quote!(::weaver_api::operations::ContextSource::Branch),
            Self::BranchName => quote!(::weaver_api::operations::ContextSource::BranchName),
            Self::Session => quote!(::weaver_api::operations::ContextSource::Session),
        }
    }
}

pub struct Operand {
    pub ident: Ident,
    pub name: String,
    pub kind: Kind,
    pub help: Option<String>,
    pub context: Option<ContextSource>,
    pub positional: bool,
    pub required_override: Option<bool>,
    pub default: Option<Expr>,
    pub long: Option<String>,
    pub short: Option<char>,
    pub skip_cli: bool,
    /// CLI-only: take this string as a file path (or stdin) rather than inline.
    pub from_file: bool,
}

impl Operand {
    /// A field the caller supplies. Context fields are filled by the dispatcher
    /// and never appear in the MCP schema or on the command line.
    pub fn is_caller_supplied(&self) -> bool {
        self.context.is_none() && !self.skip_cli
    }

    pub fn required(&self) -> bool {
        if let Some(explicit) = self.required_override {
            return explicit;
        }
        !self.kind.is_optional() && self.default.is_none() && !self.kind.is_multi()
    }

    pub fn long_flag(&self) -> String {
        self.long
            .clone()
            .unwrap_or_else(|| self.name.replace('_', "-"))
    }
}

pub fn parse(field: &Field) -> syn::Result<Operand> {
    let ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new_spanned(field, "operand structs need named fields"))?;
    let name = ident.to_string();
    let mut operand = Operand {
        kind: classify(&field.ty),
        help: doc_comment(&field.attrs),
        context: None,
        positional: false,
        required_override: None,
        default: None,
        long: None,
        short: None,
        skip_cli: false,
        from_file: false,
        name,
        ident,
    };

    for attr in &field.attrs {
        if !attr.path().is_ident("operand") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let key = meta
                .path
                .get_ident()
                .map(Ident::to_string)
                .unwrap_or_default();
            match key.as_str() {
                "context" => {
                    // `context` alone infers the source from the field name;
                    // `context = "branch"` states it.
                    let source = if meta.input.peek(syn::Token![=]) {
                        let value: LitStr = meta.value()?.parse()?;
                        ContextSource::from_str(&value.value()).ok_or_else(|| {
                            syn::Error::new_spanned(
                                &value,
                                "context must be repo_root, branch, or session",
                            )
                        })?
                    } else {
                        ContextSource::from_name(&operand.name).ok_or_else(|| {
                            meta.error(format!(
                                "cannot infer a context source from field `{}`; \
                                 write #[operand(context = \"repo_root\")]",
                                operand.name
                            ))
                        })?
                    };
                    operand.context = Some(source);
                }
                "positional" => operand.positional = true,
                "required" => operand.required_override = Some(true),
                "optional" => operand.required_override = Some(false),
                "skip_cli" => operand.skip_cli = true,
                "from_file" => operand.from_file = true,
                "json" => operand.kind = Kind::Json,
                "default" => operand.default = Some(meta.value()?.parse()?),
                "long" => {
                    let value: LitStr = meta.value()?.parse()?;
                    operand.long = Some(value.value());
                }
                "short" => {
                    let value: syn::LitChar = meta.value()?.parse()?;
                    operand.short = Some(value.value());
                }
                other => {
                    return Err(meta.error(format!("unknown operand attribute `{other}`")));
                }
            }
            Ok(())
        })?;
    }

    Ok(operand)
}

/// Classify a field's type by its surface tokens.
///
/// Deliberately syntactic: a proc macro cannot resolve aliases, so anything not
/// recognised becomes `Json` and must be handled explicitly rather than guessed.
fn classify(ty: &Type) -> Kind {
    let Type::Path(path) = ty else {
        return Kind::Json;
    };
    let Some(segment) = path.path.segments.last() else {
        return Kind::Json;
    };
    let outer = segment.ident.to_string();
    match outer.as_str() {
        "bool" => Kind::Bool,
        "String" | "str" => Kind::Str,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => Kind::Int,
        "Option" => match inner(segment) {
            Some(Kind::Int) => Kind::OptInt,
            Some(Kind::Str) => Kind::OptStr,
            // A tri-state, not a flag: `--submit` means true, `--submit=false`
            // means false, and omitting it leaves the server's default. A plain
            // `SetTrue` flag cannot express the third case.
            Some(Kind::Bool) => Kind::OptBool,
            _ => Kind::Json,
        },
        "Vec" => match inner(segment) {
            Some(Kind::Str) => Kind::VecStr,
            Some(Kind::Int) => Kind::VecInt,
            _ => Kind::Json,
        },
        _ => Kind::Json,
    }
}

fn inner(segment: &syn::PathSegment) -> Option<Kind> {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(classify(ty)),
        _ => None,
    })
}

pub fn doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let Meta::NameValue(value) = &attr.meta else {
            continue;
        };
        let Expr::Lit(expr) = &value.value else {
            continue;
        };
        let Lit::Str(text) = &expr.lit else {
            continue;
        };
        lines.push(text.value().trim().to_string());
    }
    if lines.is_empty() {
        return None;
    }
    // Help text is one paragraph; blank doc lines separate the summary from
    // prose the CLI renders as the long help.
    Some(lines.join(" ").trim().to_string())
}

/// Emit the clap argument builder for one operand.
pub fn clap_arg(operand: &Operand) -> TokenStream {
    let name = &operand.name;
    let long = operand.long_flag();
    let help = operand.help.clone().unwrap_or_default();
    let required = operand.required();

    let base = if operand.positional || operand.from_file {
        quote!(::clap::Arg::new(#name))
    } else {
        let mut builder = quote!(::clap::Arg::new(#name).long(#long));
        if let Some(short) = operand.short {
            builder = quote!(#builder.short(#short));
        }
        builder
    };

    let action = match operand.kind {
        Kind::Bool => quote!(.action(::clap::ArgAction::SetTrue)),
        Kind::OptBool => quote!(
            .num_args(0..=1)
            .default_missing_value("true")
            .action(::clap::ArgAction::Set)
        ),
        Kind::VecStr | Kind::VecInt => {
            if operand.positional {
                quote!(.num_args(0..).action(::clap::ArgAction::Append))
            } else {
                quote!(.action(::clap::ArgAction::Append))
            }
        }
        _ => quote!(.action(::clap::ArgAction::Set)),
    };

    let parser = match operand.kind {
        Kind::Int | Kind::OptInt | Kind::VecInt => {
            quote!(.value_parser(::clap::value_parser!(i64)))
        }
        Kind::OptBool => quote!(.value_parser(::clap::value_parser!(bool))),
        Kind::Bool => quote!(),
        _ => quote!(.value_parser(::clap::value_parser!(String))),
    };

    let required = if required && !operand.kind.is_multi() && !operand.from_file {
        quote!(.required(true))
    } else {
        quote!()
    };

    let help = if help.is_empty() {
        quote!()
    } else {
        quote!(.help(#help))
    };

    quote!(cmd = cmd.arg(#base #action #parser #required #help);)
}

/// Emit the expression that reconstructs one operand from parsed matches.
pub fn from_matches(operand: &Operand) -> TokenStream {
    let ident = &operand.ident;
    let name = &operand.name;

    // Context fields are absent from the command line by construction; they are
    // populated by `fill_context` before the request is sent.
    if operand.context.is_some() || operand.skip_cli {
        return quote!(#ident: ::core::default::Default::default(),);
    }

    if operand.from_file {
        let ident = &operand.ident;
        return quote! {
            #ident: {
                use ::std::io::Read as _;
                match matches.get_one::<String>(#name).map(String::as_str) {
                    Some(path) if path != "-" => ::std::fs::read_to_string(path)
                        .map_err(|error| format!("reading {path}: {error}"))?,
                    _ => {
                        let mut buffer = String::new();
                        ::std::io::stdin()
                            .read_to_string(&mut buffer)
                            .map_err(|error| format!("reading stdin: {error}"))?;
                        buffer
                    }
                }
            },
        };
    }

    let value = match operand.kind {
        Kind::Bool => quote!(matches.get_flag(#name)),
        Kind::Str => match &operand.default {
            Some(default) => quote!(matches
                .get_one::<String>(#name)
                .cloned()
                .unwrap_or_else(|| (#default).into())),
            None => quote!(matches
                .get_one::<String>(#name)
                .cloned()
                .ok_or_else(|| format!("missing required argument --{}", #name))?),
        },
        Kind::OptStr => quote!(matches.get_one::<String>(#name).cloned()),
        Kind::OptBool => quote!(matches.get_one::<bool>(#name).copied()),
        Kind::Int => {
            let convert = quote!(::core::convert::TryFrom::try_from(raw)
                .map_err(|_| format!("--{} is out of range", #name))?);
            match &operand.default {
                Some(default) => quote!(match matches.get_one::<i64>(#name).copied() {
                    Some(raw) => #convert,
                    None => (#default),
                }),
                None => quote!({
                    let raw = matches
                        .get_one::<i64>(#name)
                        .copied()
                        .ok_or_else(|| format!("missing required argument --{}", #name))?;
                    #convert
                }),
            }
        }
        Kind::OptInt => quote!(match matches.get_one::<i64>(#name).copied() {
            Some(raw) => Some(
                ::core::convert::TryFrom::try_from(raw)
                    .map_err(|_| format!("--{} is out of range", #name))?,
            ),
            None => None,
        }),
        Kind::VecStr => quote!(matches
            .get_many::<String>(#name)
            .map(|values| values.cloned().collect())
            .unwrap_or_default()),
        Kind::VecInt => quote!({
            let mut collected = ::std::vec::Vec::new();
            if let Some(values) = matches.get_many::<i64>(#name) {
                for raw in values.copied() {
                    collected.push(
                        ::core::convert::TryFrom::try_from(raw)
                            .map_err(|_| format!("--{} is out of range", #name))?,
                    );
                }
            }
            collected
        }),
        // A `Json` operand takes one JSON literal on the command line. Explicit
        // and ugly on purpose: it marks the operands a flag cannot express.
        Kind::Json => match &operand.default {
            Some(default) => quote!(match matches.get_one::<String>(#name) {
                Some(raw) => ::serde_json::from_str(raw)
                    .map_err(|error| format!("--{} is not valid JSON: {error}", #name))?,
                None => (#default),
            }),
            None => quote!({
                let raw = matches
                    .get_one::<String>(#name)
                    .ok_or_else(|| format!("missing required argument --{}", #name))?;
                ::serde_json::from_str(raw)
                    .map_err(|error| format!("--{} is not valid JSON: {error}", #name))?
            }),
        },
    };

    quote!(#ident: #value,)
}
