//! The generic CLI dispatcher.
//!
//! There is no per-command code here and no per-command code anywhere else. The
//! clap tree is *built from* the registry, so an advertised invocation and the
//! parser that accepts it are the same declaration read twice. The surface this
//! replaces kept a `cli: Some("loom issues list")` string beside a clap enum
//! whose variant was `Ls`, and shipped three advertised commands that did not
//! exist.

use anyhow::{anyhow, Result};
use clap::{ArgMatches, Command};
use weaver_api::operations::{
    ContextValues, Operands, Operation, OperationSpec, Render, ViewFlags,
};
use weaver_api::Client;

/// One registered operation's command-line binding.
///
/// The closures are the only per-operation code, and they are produced by a
/// generic function over the operation's own types — so they cannot disagree
/// with the descriptor they sit beside.
#[derive(Clone, Copy)]
pub struct CliBinding {
    pub operation: &'static OperationSpec,
    /// Add this operation's operands and view flags to its leaf command.
    pub augment: fn(Command) -> Command,
    /// Parse, invoke, and render. Returns the text to print.
    pub run: fn(&ArgMatches) -> BoxFuture,
}

type BoxFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>;

/// Build a binding for one operation from its types alone.
pub fn bind<O>() -> CliBinding
where
    O: Operation + Render,
    O::Input: Send,
{
    CliBinding {
        operation: O::SPEC,
        augment: |cmd| {
            let cmd = <O::Input as Operands>::augment(cmd);
            <O::View as ViewFlags>::augment(cmd)
        },
        run: |matches| {
            // Parse before any I/O so a bad invocation fails fast and locally.
            let parsed = <O::Input as Operands>::from_matches(matches)
                .map_err(|error| anyhow!(error))
                .and_then(|input| {
                    let view = <O::View as ViewFlags>::from_matches(matches)
                        .map_err(|error| anyhow!(error))?;
                    Ok((input, view))
                });
            Box::pin(async move {
                let (mut input, view) = parsed?;
                let client = crate::agent_cli::client();
                // Context is resolved once, here — not inside each command, and
                // not by an extra round-trip per MCP tool as the old adapters did.
                if !<O::Input as Operands>::CONTEXT.is_empty() {
                    let context = resolve_context(&client).await?;
                    input.fill_context(&context);
                }
                let output = client.invoke::<O>(&input).await?;
                Ok(O::text(&output, &view))
            })
        },
    }
}

async fn resolve_context(client: &Client) -> Result<ContextValues> {
    let context = client.self_context().await?;
    Ok(ContextValues {
        repo_root: context.repo_root,
        branch: context.branch_id,
        branch_name: context.branch_name,
        session: context.session_id,
    })
}

/// Assemble the registered command tree.
///
/// Merges into whatever is already there. A group the host already defines
/// (`loom issues`, `loom settings`) keeps its hand-written subcommands and gains
/// the declared ones it was missing — which is how `loom issues list` comes to
/// exist beside the hand-written `ls`, instead of the registry advertising a
/// command nobody implemented.
pub fn augment(root: Command, bindings: &[CliBinding]) -> Command {
    let mut root = root;
    for binding in bindings {
        let Some(cli) = binding.operation.cli else {
            continue;
        };
        root = insert(root, cli.group(), *binding);
    }
    root
}

/// Descend to `path` and add the binding's leaf there.
///
/// The leaf is built at the insertion point rather than beforehand, because
/// whether its name and aliases are free depends on what is already in that
/// group — and clap treats a duplicate name *or* alias as a bug and panics.
fn insert(command: Command, path: &[&'static str], binding: CliBinding) -> Command {
    let Some((head, rest)) = path.split_first() else {
        return place(command, binding);
    };
    if command
        .get_subcommands()
        .any(|existing| existing.get_name() == *head)
    {
        command.mut_subcommand(*head, |sub| insert(sub, rest, binding))
    } else {
        command.subcommand(insert(Command::new(*head), rest, binding))
    }
}

/// Add one leaf to the group it belongs in, yielding to what is already there.
///
/// A hand-written command that already answers to this name wins: it exists
/// because someone wanted its output formatted by hand, and this must not
/// shadow it. Aliases are filtered the same way — `issues.list` advertises the
/// alias `ls`, which the hand-written `issues` command already uses.
fn place(command: Command, binding: CliBinding) -> Command {
    let Some(cli) = binding.operation.cli else {
        return command;
    };
    let taken: Vec<String> = command
        .get_subcommands()
        .flat_map(|existing| {
            std::iter::once(existing.get_name().to_string())
                .chain(existing.get_all_aliases().map(str::to_string))
        })
        .collect();
    if taken.iter().any(|name| name == cli.leaf()) {
        return command;
    }
    let mut leaf = Command::new(cli.leaf()).about(binding.operation.summary);
    for alias in cli.aliases {
        if !taken.iter().any(|name| name == alias) {
            leaf = leaf.visible_alias(*alias);
        }
    }
    command.subcommand((binding.augment)(leaf))
}

/// Resolve parsed matches to the binding they named, walking the command path.
pub fn resolve<'a>(
    bindings: &'a [CliBinding],
    matches: &ArgMatches,
) -> Option<(&'a CliBinding, ArgMatches)> {
    for binding in bindings {
        // A registered operation with no CLI projection simply is not reachable
        // from the command line; skip it rather than abandoning the search.
        let Some(cli) = binding.operation.cli else {
            continue;
        };
        let mut current = matches.clone();
        let mut matched = true;
        for segment in cli.path {
            match current.subcommand() {
                Some((name, next)) if name == *segment => current = next.clone(),
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return Some((binding, current));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that the previous design could not state: every advertised
    /// invocation is accepted by the parser, because they are the same value.
    #[test]
    fn every_advertised_invocation_parses() {
        let bindings = crate::cli::bindings();
        let root = augment(Command::new("loom").no_binary_name(true), &bindings);
        for binding in &bindings {
            let Some(cli) = binding.operation.cli else {
                continue;
            };
            let mut command = &root;
            for segment in cli.path {
                command = command
                    .get_subcommands()
                    .find(|candidate| {
                        candidate.get_name() == *segment
                            || candidate.get_all_aliases().any(|alias| alias == *segment)
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{} advertises `{}` but `{segment}` is not a command",
                            binding.operation.id,
                            cli.invocation()
                        )
                    });
            }
        }
    }
}
