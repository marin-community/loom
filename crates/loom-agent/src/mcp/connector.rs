//! The tools Loom's remote MCP endpoint offers a person, such as through the
//! claude.ai connector.
//!
//! The in-session server ([`super::serve`]) is agent-facing: it runs inside a
//! session with that session's token. This list is its sibling for people.
//! Each tool is a [`UserExport`] of a registered operation, checked at compile
//! time to be one a person may call, and invoked under the caller's own
//! credential by the transport in `loom::web::mcp`. The list starts from the
//! session operations already reviewed for Loom's credentialed browser
//! embedding.
//!
//! Deliberately absent: `sessions.wait` and `channels.wait`, because connector
//! calls time out and `launch` already returns the session id; and
//! `sessions.permissions.answer`, until Loom decides whether an outside
//! assistant may approve an agent's permission requests for a person.

use std::sync::OnceLock;

use serde_json::{json, Value};
use weaver_api::operations::{channels, sessions, OperationRisk};
use weaver_core::config::CONNECTOR_DEFAULT_PROFILE_KEY;

use super::dispatch::export_user;
pub use super::dispatch::UserExport;

/// Guidance sent with `initialize`, so an assistant knows how the tools chain
/// together without reading every description.
pub const INSTRUCTIONS: &str = "Loom runs coding agents in isolated worktrees and \
reports their progress. Hand a task to Loom with `launch`: put the person's request \
verbatim in `goal`, followed by acceptance criteria and anything known about where \
to look. Then call `url` with the new session id and give the person that link. \
Check on a session with `get` and `status`; read its outcome with `messages`, \
passing the session id as `channel` and `kinds: [\"result\"]`. Send follow-up \
instructions with `send`, stop the current turn with `interrupt`, and `archive` a \
session only when asked.";

const LAUNCH: &str = "Start a Loom session: an agent works on `goal` in its own \
worktree of `repo` (GitHub `owner/name`) and reports status and a result. Returns \
the session; pass its `id` to the other tools. `title` is a one-line label. `base` \
defaults to the repository's default branch, and `profile` to the connector's \
launch profile.";

const LIST: &str = "List the sessions visible to the person, one compact row each. \
Use `q` to search titles, goals and branches, `creator` to narrow to the person's \
own sessions, and `archived` to include finished work.";

const MESSAGES: &str = "Read a session's channel. Pass the session id as `channel`; \
`kinds: [\"result\"]` returns only the outcomes the agent reported, and `after` \
skips messages already seen.";

/// The tools, in the order they are advertised.
pub fn exports() -> &'static [UserExport] {
    static EXPORTS: OnceLock<Vec<UserExport>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export_user::<sessions::launch::Op>("launch")
                .describe(LAUNCH)
                .only(&["title", "goal", "repo", "base", "profile"])
                .default_from_settings(&[("profile", CONNECTOR_DEFAULT_PROFILE_KEY)]),
            export_user::<sessions::summary::list::Op>("list").describe(LIST),
            export_user::<sessions::get::Op>("get"),
            export_user::<sessions::status::get::Op>("status"),
            export_user::<channels::messages::list::Op>("messages")
                .describe(MESSAGES)
                .only(&["after", "limit", "kinds", "peek"]),
            export_user::<sessions::send::Op>("send").only(&["text"]),
            export_user::<sessions::interrupt::Op>("interrupt"),
            export_user::<sessions::url::Op>("url"),
            export_user::<sessions::archive::Op>("archive"),
        ]
    })
}

pub fn lookup(tool: &str) -> Option<&'static UserExport> {
    exports().iter().find(|export| export.tool == tool)
}

/// The advertised catalogue, with MCP tool annotations derived from each
/// operation's declared risk.
pub fn tools() -> Value {
    Value::Array(
        exports()
            .iter()
            .map(|export| {
                json!({
                    "name": export.tool,
                    "description": export.description(),
                    "inputSchema": export.input_schema(),
                    "annotations": annotations(export.operation.risk),
                })
            })
            .collect(),
    )
}

fn annotations(risk: OperationRisk) -> Value {
    match risk {
        OperationRisk::Read => json!({ "readOnlyHint": true, "openWorldHint": false }),
        OperationRisk::Write => json!({
            "readOnlyHint": false,
            "destructiveHint": false,
            "openWorldHint": false,
        }),
        OperationRisk::Destructive => json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "openWorldHint": false,
        }),
        OperationRisk::ExternalWrite => json!({
            "readOnlyHint": false,
            "destructiveHint": false,
            "openWorldHint": true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_the_reviewed_tools_in_order() {
        let names: Vec<&str> = exports().iter().map(|export| export.tool).collect();
        assert_eq!(
            names,
            [
                "launch",
                "list",
                "get",
                "status",
                "messages",
                "send",
                "interrupt",
                "url",
                "archive"
            ]
        );
        assert_eq!(tools().as_array().unwrap().len(), names.len());
    }

    #[test]
    fn launch_offers_only_the_person_facing_fields() {
        let schema = lookup("launch").unwrap().input_schema();
        let mut fields: Vec<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(fields, ["base", "goal", "profile", "repo", "title"]);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            lookup("launch").unwrap().setting_defaults(),
            [("profile", CONNECTOR_DEFAULT_PROFILE_KEY)]
        );
    }

    #[test]
    fn a_person_names_the_session_or_channel_explicitly() {
        for tool in ["get", "status", "send", "interrupt", "url", "archive"] {
            let schema = lookup(tool).unwrap().input_schema();
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("session")),
                "{tool}: {schema}"
            );
            assert_eq!(schema["properties"]["session"]["type"], "string");
        }
        let messages = lookup("messages").unwrap().input_schema();
        assert!(messages["required"]
            .as_array()
            .unwrap()
            .contains(&json!("channel")));
        // The reader's own branch is context for an agent and never a person's
        // argument.
        assert!(messages["properties"].get("branch").is_none());
    }

    #[test]
    fn prepare_leaves_context_empty_and_rejects_unadvertised_arguments() {
        let launch = lookup("launch").unwrap();
        let input = launch
            .prepare(json!({ "goal": "tidy the README", "repo": "octo/repo" }))
            .unwrap();
        assert_eq!(input["goal"], "tidy the README");
        assert_eq!(input["parent_branch"], Value::Null);
        for smuggled in [
            json!({ "goal": "x", "parent_branch": "abc" }),
            json!({ "goal": "x", "cwd": "/etc" }),
            json!({ "goal": "x", "selection": {} }),
        ] {
            let error = launch.prepare(smuggled).unwrap_err().to_string();
            assert!(error.contains("does not accept"), "{error}");
        }

        let get = lookup("get").unwrap();
        assert!(get
            .prepare(json!({}))
            .unwrap_err()
            .to_string()
            .contains("missing required argument(s): session"));
        assert_eq!(
            get.prepare(json!({ "session": "abc" })).unwrap(),
            json!({ "session": "abc" })
        );
        assert!(get.prepare(json!(["abc"])).is_err());
    }

    #[test]
    fn annotations_follow_declared_risk() {
        let tools = tools();
        let by_name = |name: &str| {
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()["annotations"]
                .clone()
        };
        assert_eq!(by_name("get")["readOnlyHint"], true);
        assert_eq!(by_name("launch")["readOnlyHint"], false);
    }
}
