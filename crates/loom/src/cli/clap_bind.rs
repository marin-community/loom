//! The command line, built from an operation's operand list.
//!
//! This is the whole of Loom's clap knowledge, written once and driven by
//! data, so `weaver-api` — the crate the server, the Python binding, and
//! every embedder link to merely *describe* an operation — never depends on
//! a command-line parser.
//!
//! An [`Operand`] says what a field is called, what kind of value it holds, whether a
//! caller must supply it, what it defaults to, and how the command line spells
//! it. That is enough to build the parser and to read the parse back out, so
//! nothing about the command line has to be written down twice.
//!
//! [`from_matches`] produces the JSON an operand struct deserializes from
//! rather than the struct itself. Building the struct field by field needs the
//! field's type at compile time; going through JSON avoids that, so one
//! runtime function serves all 214 operations, and a malformed value is
//! reported by `serde` — the same place REST and MCP report it.

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value};
use weaver_api::operations::{CliSpelling, Operand, OperandKind};

/// Add every command-line operand in `operands` to `cmd`.
pub fn augment(mut cmd: Command, operands: &'static [Operand]) -> Command {
    for operand in operands {
        let Some(cli) = operand.cli else { continue };
        cmd = cmd.arg(arg(operand, cli));
    }
    cmd
}

fn arg(operand: &Operand, cli: CliSpelling) -> Arg {
    // A positional takes no flag, and neither does a `from_file` operand: it
    // names a path or `-`, which reads better as `loom artifacts write x ./body.md`.
    let mut arg = if cli.positional || cli.from_file {
        Arg::new(operand.name)
    } else {
        let mut arg = Arg::new(operand.name).long(cli.long);
        if let Some(short) = cli.short {
            arg = arg.short(short);
        }
        arg
    };

    arg = match operand.kind {
        OperandKind::Bool => arg.action(ArgAction::SetTrue),
        // Tri-state: `--submit` enables it, `--submit=false` disables it, and
        // omitting it leaves the server's default alone.
        OperandKind::OptBool => arg
            .num_args(0..=1)
            .default_missing_value("true")
            .action(ArgAction::Set)
            .value_parser(clap::value_parser!(bool)),
        OperandKind::VecStr | OperandKind::VecInt => {
            let arg = if cli.positional {
                arg.num_args(0..)
            } else {
                arg
            };
            arg.action(ArgAction::Append)
                .value_parser(value_parser(operand.kind))
        }
        _ => arg
            .action(ArgAction::Set)
            .value_parser(value_parser(operand.kind)),
    };

    // A repeated operand is never `required`: an empty list is a legitimate
    // answer. A `from_file` operand is never required either — omitting it
    // means stdin.
    if operand.required && !operand.kind.is_multi() && !cli.from_file {
        arg = arg.required(true);
    }
    if let Some(help) = operand.help {
        arg = arg.help(help);
    }
    // `--help` shows what omitting the flag will do, e.g. that `kind` defaults
    // to `message`. Only scalars, and not a bare `--flag`, whose absence
    // already reads as false: a default that renders as `{}` or `[]` is noise
    // rather than guidance.
    if operand.kind != OperandKind::Bool {
        if let Some(shown) = operand
            .default
            .map(|default| default())
            .and_then(scalar_text)
        {
            arg = arg.default_value(clap::builder::Str::from(shown));
        }
    }
    arg
}

fn value_parser(kind: OperandKind) -> clap::builder::ValueParser {
    match kind {
        OperandKind::Int | OperandKind::OptInt | OperandKind::VecInt => {
            clap::value_parser!(i64).into()
        }
        _ => clap::value_parser!(String),
    }
}

/// The default as `--help` should show it, or `None` if showing it teaches
/// nothing: an object, a list, or the empty string that already reads as
/// "unset" in the flag's own help text.
fn scalar_text(value: Value) -> Option<String> {
    match value {
        Value::String(text) if text.is_empty() => None,
        Value::String(text) => Some(text),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Read parsed matches back as the JSON object an operand struct decodes from.
///
/// Every caller-supplied field is written, including the ones the caller left
/// out, so a view struct — which has no `serde` defaults of its own — decodes
/// as readily as an operation `Input`. Context fields are omitted: the
/// dispatcher fills them from the session afterwards.
pub fn from_matches(matches: &ArgMatches, operands: &'static [Operand]) -> Result<Value, String> {
    let mut object = Map::new();
    for operand in operands {
        let Some(cli) = operand.cli else { continue };
        if let Some(value) = read(matches, operand, cli)? {
            object.insert(operand.name.to_string(), value);
        }
    }
    Ok(Value::Object(object))
}

fn read(
    matches: &ArgMatches,
    operand: &Operand,
    cli: CliSpelling,
) -> Result<Option<Value>, String> {
    if cli.from_file {
        return Ok(Some(Value::String(read_file(matches, operand.name)?)));
    }
    let supplied = match operand.kind {
        OperandKind::Bool => matches.get_flag(operand.name).then_some(Value::Bool(true)),
        OperandKind::OptBool => matches
            .get_one::<bool>(operand.name)
            .map(|flag| Value::Bool(*flag)),
        OperandKind::Int | OperandKind::OptInt => matches
            .get_one::<i64>(operand.name)
            .map(|number| Value::Number((*number).into())),
        OperandKind::VecInt => matches.get_many::<i64>(operand.name).map(|values| {
            Value::Array(
                values
                    .map(|number| Value::Number((*number).into()))
                    .collect(),
            )
        }),
        OperandKind::VecStr => matches
            .get_many::<String>(operand.name)
            .map(|values| Value::Array(values.map(|text| Value::String(text.clone())).collect())),
        // A `Json` operand takes one JSON literal. Explicit and ugly on
        // purpose: it marks the operands a flag cannot express.
        OperandKind::Json => match matches.get_one::<String>(operand.name) {
            Some(raw) => Some(
                serde_json::from_str(raw)
                    .map_err(|error| format!("--{} is not valid JSON: {error}", cli.long))?,
            ),
            None => None,
        },
        OperandKind::Str | OperandKind::OptStr => matches
            .get_one::<String>(operand.name)
            .map(|text| Value::String(text.clone())),
    };
    if let Some(value) = supplied {
        return Ok(Some(value));
    }
    if let Some(default) = operand.default {
        return Ok(Some(default()));
    }
    // What an absent field means, when nothing declared it: no value, no
    // entries, not set. A required scalar that reaches here was already
    // refused by clap.
    Ok(match operand.kind {
        OperandKind::OptBool | OperandKind::OptInt | OperandKind::OptStr => Some(Value::Null),
        OperandKind::VecInt | OperandKind::VecStr => Some(Value::Array(Vec::new())),
        OperandKind::Bool => Some(Value::Bool(false)),
        _ => None,
    })
}

/// Read a `from_file` operand: the named file, or stdin for `-` or nothing.
fn read_file(matches: &ArgMatches, name: &str) -> Result<String, String> {
    use std::io::Read as _;
    match matches.get_one::<String>(name).map(String::as_str) {
        Some(path) if path != "-" => {
            std::fs::read_to_string(path).map_err(|error| format!("reading {path}: {error}"))
        }
        _ => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|error| format!("reading stdin: {error}"))?;
            Ok(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaver_api::operations::{issues, Operands, ViewFlags};

    /// A caller who supplies nothing still produces a decodable object: the
    /// declared defaults fill it, which is what lets a view struct — with no
    /// `serde` attributes of its own — round-trip.
    #[test]
    fn absent_operands_decode_to_their_declared_defaults() {
        let command = augment(
            augment(
                Command::new("list").no_binary_name(true),
                issues::list::Input::OPERANDS,
            ),
            issues::list::View::OPERANDS,
        );
        let matches = command.try_get_matches_from(Vec::<String>::new()).unwrap();

        let input: issues::list::Input =
            serde_json::from_value(from_matches(&matches, issues::list::Input::OPERANDS).unwrap())
                .unwrap();
        assert!(!input.all);
        assert!(
            input.repo_root.is_empty(),
            "context is filled by the dispatcher"
        );

        let view: issues::list::View =
            serde_json::from_value(from_matches(&matches, issues::list::View::OPERANDS).unwrap())
                .unwrap();
        assert!(!view.mine);
    }

    #[test]
    fn supplied_operands_win_over_defaults() {
        let command = augment(
            Command::new("list").no_binary_name(true),
            issues::list::Input::OPERANDS,
        );
        let matches = command.try_get_matches_from(["--all"]).unwrap();
        let input: issues::list::Input =
            serde_json::from_value(from_matches(&matches, issues::list::Input::OPERANDS).unwrap())
                .unwrap();
        assert!(input.all);
    }
}
