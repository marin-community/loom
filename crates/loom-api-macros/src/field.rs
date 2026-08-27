//! Field-level parsing shared by the `Operands` and `View` derives.
//!
//! Both derives turn a struct into command-line arguments, so the attribute vocabulary and
//! type classification live here rather than being written twice with a
//! chance to disagree.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Expr, Field, Ident, Lit, LitStr, Meta, Type};

/// How a field's Rust type projects onto a command line.
///
/// Anything the CLI cannot express as a flag or positional lands in `Json`,
/// which is deliberately explicit: operands with complex types must declare
/// themselves.
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
        matches!(
            self,
            Kind::OptInt | Kind::OptStr | Kind::OptBool | Kind::Bool
        )
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
    /// CLI-only: interpret this string as a file path or stdin.
    pub from_file: bool,
    /// The declared field type, used to type-annotate the declared default so a
    /// bare `None` or `BTreeMap::new()` still infers.
    pub ty: syn::Type,
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
        !self.kind.is_optional() && self.default.is_none() && !self.is_list()
    }

    /// Any `Vec<_>`, whatever it holds.
    ///
    /// `Kind::is_multi` only recognizes the two element types the command line
    /// can repeat; other `Vec<_>` types classify as `Json`, which without this
    /// check would count as required, forcing a caller to pass an argument for
    /// a filter whose absence should mean "no filter".
    fn is_list(&self) -> bool {
        if self.kind.is_multi() {
            return true;
        }
        let Type::Path(path) = &self.ty else {
            return false;
        };
        path.path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Vec")
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
        ty: field.ty.clone(),
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
                // An enum that serializes to JSON as a bare string, but
                // [`classify`] is syntactic and can only see an unfamiliar
                // type name, so it lands in `Json` and the command line
                // demands the JSON spelling `'"space"'` for a value a user
                // writes as `space`.
                "string" => operand.kind = Kind::Str,
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

/// The outermost type constructor's name, as written.
///
/// Syntactic like [`classify`], and used for the same reason: to decide whether
/// a field has a meaningful empty value (`None`, `[]`, `false`) that a caller
/// may leave out.
pub fn outer_ident(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

/// Classify a field's type from its written syntax; deliberately does not
/// resolve aliases, since a proc macro cannot. Unrecognized types become
/// `Json` and require explicit handling.
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
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            Kind::Int
        }
        "Option" => match inner(segment) {
            Some(Kind::Int) => Kind::OptInt,
            Some(Kind::Str) => Kind::OptStr,
            // A tri-state option: `--submit` enables it, `--submit=false`
            // disables it, and omitting it uses the server's default.
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

/// Extract the first paragraph of a `///` doc comment.
///
/// This becomes an operation's MCP description, its OpenAPI `summary`, and an
/// operand's schema/CLI help text — everything an API caller sees. A doc
/// comment can say more after a blank `///` line (grant reasoning, an
/// internal cross-reference, why a type is designed the way it is); that
/// stays ordinary rustdoc for whoever reads the source, and stops there
/// instead of leaking into what a caller sees.
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
        let line = text.value().trim().to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines.join(" ").trim().to_string())
}

/// One entry of the `Operands::OPERANDS` list.
///
/// The command line is built from this at runtime by `loom::cli::clap_bind`,
/// which is why nothing here mentions `clap`.
pub fn operand_entry(operand: &Operand) -> TokenStream {
    let name = &operand.name;
    let kind = match operand.kind {
        Kind::Bool => quote!(Bool),
        Kind::Int => quote!(Int),
        Kind::Str => quote!(Str),
        Kind::OptBool => quote!(OptBool),
        Kind::OptInt => quote!(OptInt),
        Kind::OptStr => quote!(OptStr),
        Kind::VecStr => quote!(VecStr),
        Kind::VecInt => quote!(VecInt),
        Kind::Json => quote!(Json),
    };
    let help = match &operand.help {
        Some(help) => quote!(::core::option::Option::Some(#help)),
        None => quote!(::core::option::Option::None),
    };
    let required = operand.required();
    let context = match operand.context {
        Some(source) => {
            let source = source.tokens();
            quote!(::core::option::Option::Some(#source))
        }
        None => quote!(::core::option::Option::None),
    };
    let default = match &operand.default {
        Some(default) => {
            let ty = &operand.ty;
            quote!(::core::option::Option::Some(|| {
                ::serde_json::to_value::<#ty>(::core::convert::Into::into(#default))
                    .unwrap_or(::serde_json::Value::Null)
            }))
        }
        None => quote!(::core::option::Option::None),
    };
    let cli = if operand.is_caller_supplied() {
        let positional = operand.positional;
        let long = operand.long_flag();
        let short = match operand.short {
            Some(short) => quote!(::core::option::Option::Some(#short)),
            None => quote!(::core::option::Option::None),
        };
        let from_file = operand.from_file;
        quote!(::core::option::Option::Some(::weaver_api::operations::CliSpelling {
            positional: #positional,
            long: #long,
            short: #short,
            from_file: #from_file,
        }))
    } else {
        quote!(::core::option::Option::None)
    };
    quote! {
        ::weaver_api::operations::Operand {
            name: #name,
            kind: ::weaver_api::operations::OperandKind::#kind,
            help: #help,
            required: #required,
            context: #context,
            default: #default,
            cli: #cli,
        }
    }
}
