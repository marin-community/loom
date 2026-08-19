//! The generic CLI dispatcher.
//!
//! There is no per-command code here and no per-command code anywhere else. The
//! clap tree is *built from* the registry, so an advertised invocation and the
//! parser that accepts it are the same declaration read twice. The surface this
//! replaces kept a `cli: Some("loom issues list")` string beside a clap enum
//! whose variant was `Ls`, and shipped three advertised commands that did not
//! exist.

use std::collections::BTreeMap;

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
                let client = crate::agent_cli::client()?;
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
        session: context.session_id.unwrap_or_default(),
    })
}

/// Assemble the registered command tree.
///
/// Groups nest by the declared command path, so `issues tag set` produces
/// `issues` -> `tag` -> `set` without anyone writing a nested clap enum.
pub fn augment(root: Command, bindings: &[CliBinding]) -> Command {
    // Every node of the command tree, keyed by its path. The empty path holds
    // flat top-level commands such as `loom summary`.
    let mut nodes: BTreeMap<Vec<&'static str>, Command> = BTreeMap::new();

    let attach = |nodes: &mut BTreeMap<Vec<&'static str>, Command>,
                  path: &Vec<&'static str>,
                  child: Command| {
        let name = path.last().copied().unwrap_or("loom");
        let entry = nodes
            .entry(path.clone())
            .or_insert_with(|| Command::new(name));
        // clap builders are owned, so swap the node out to consume it.
        let taken = std::mem::replace(entry, Command::new(name));
        *entry = taken.subcommand(child);
    };

    for binding in bindings {
        let Some(cli) = binding.operation.cli else {
            continue;
        };
        let mut leaf = Command::new(cli.leaf()).about(binding.operation.summary);
        for alias in cli.aliases {
            leaf = leaf.visible_alias(*alias);
        }
        leaf = (binding.augment)(leaf);

        let group = cli.group().to_vec();
        for depth in 1..group.len() {
            let ancestor = group[..depth].to_vec();
            let name = ancestor.last().copied().unwrap_or("loom");
            nodes.entry(ancestor).or_insert_with(|| Command::new(name));
        }
        attach(&mut nodes, &group, leaf);
    }

    // Fold deepest-first so a child is complete before its parent consumes it.
    let mut paths: Vec<_> = nodes.keys().cloned().collect();
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));

    let mut root = root;
    for path in paths {
        let Some(command) = nodes.remove(&path) else {
            continue;
        };
        match path.len() {
            // Flat commands: splice straight onto the root.
            0 => {
                for sub in command.get_subcommands().cloned().collect::<Vec<_>>() {
                    root = root.subcommand(sub);
                }
            }
            1 => root = root.subcommand(command),
            _ => {
                let parent = path[..path.len() - 1].to_vec();
                attach(&mut nodes, &parent, command);
            }
        }
    }
    root
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
