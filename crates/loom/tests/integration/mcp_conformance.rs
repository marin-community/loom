//! Real-process conformance for every trusted builtin MCP adapter.

use std::process::Stdio;

use base64::Engine as _;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

use crate::fixtures::TestServer;

fn loom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loom")
}

async fn run_adapter(adapter: &str) -> (Vec<Value>, std::process::ExitStatus) {
    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve", adapter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\"}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"unknown/method\",\"params\":{}}\n",
        )
        .await
        .unwrap();
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"not_registered\",\"arguments\":{}}}\n",
        )
        .await
        .unwrap();
    drop(stdin);
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut values = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        values.push(serde_json::from_str(&line).unwrap());
    }
    let status = child.wait().await.unwrap();
    (values, status)
}

async fn run_filtered_adapter(adapter: &str, tools: &[&str]) -> Vec<Value> {
    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve", adapter])
        .env(
            "LOOM_MCP_ALLOWED_TOOLS",
            serde_json::to_string(tools).unwrap(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"issue_edit\",\"arguments\":{}}}\n",
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut values = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        values.push(serde_json::from_str(&line).unwrap());
    }
    assert!(child.wait().await.unwrap().success());
    values
}

#[tokio::test]
async fn every_builtin_speaks_mcp_stdio() {
    for (adapter, expected_tools) in [
        ("github", 6),
        ("context", 1),
        ("channel", 8),
        ("artifact", 8),
        ("session", 4),
        ("history", 2),
        ("messaging", 2),
    ] {
        let (values, status) = run_adapter(adapter).await;
        assert!(status.success(), "{adapter} did not exit cleanly");
        assert_eq!(values.len(), 4, "{adapter} replied to a notification");
        assert_eq!(values[0]["id"], 1);
        assert_eq!(values[1]["id"], 2);
        assert_eq!(
            values[1]["result"]["tools"].as_array().unwrap().len(),
            expected_tools
        );
        assert_eq!(
            values[2],
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "error": { "code": -32601, "message": "method not found: unknown/method" }
            })
        );
        assert_eq!(values[3]["id"], 4);
        assert_eq!(values[3]["result"]["isError"], true);
        assert!(values[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown"));
    }
}

#[tokio::test]
async fn malformed_json_fails_closed_and_clean_eof_succeeds() {
    let clean = Command::new(loom_bin())
        .args(["mcp", "serve", "github"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(clean.success());

    let mut malformed = Command::new(loom_bin())
        .args(["mcp", "serve", "messaging"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    malformed
        .stdin
        .take()
        .unwrap()
        .write_all(b"not json\n")
        .await
        .unwrap();
    let output = malformed.wait_with_output().await.unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid MCP"));
}

#[tokio::test]
async fn builtin_process_exposes_only_the_session_stamped_tools() {
    let values = run_filtered_adapter("github", &["issue_view"]).await;
    assert_eq!(
        values[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["issue_view"]
    );
    assert_eq!(values[1]["result"]["isError"], true);
    assert!(values[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not allowed by this session"));
}

#[tokio::test]
async fn custom_proxy_enforces_the_profile_stamped_tool_surface() {
    let source = r#"
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if request.get("method") == "tools/list":
        print(json.dumps({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"tools": [
                {"name": "allowed", "inputSchema": {"type": "object"}},
                {"name": "surprise", "inputSchema": {"type": "object"}},
            ]},
        }), flush=True)
"#;
    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve-custom", "/tests/filter"])
        .env(
            "LOOM_CUSTOM_MCP_SOURCE_B64",
            base64::engine::general_purpose::STANDARD.encode(source),
        )
        .env("LOOM_MCP_ALLOWED_TOOLS", "[\"allowed\"]")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"surprise\",\"arguments\":{}}}\n",
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut values = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        values.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    assert!(child.wait().await.unwrap().success());

    let listed = values.iter().find(|value| value["id"] == 1).unwrap();
    assert_eq!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["allowed"]
    );
    let rejected = values.iter().find(|value| value["id"] == 2).unwrap();
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not allowed by this session"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messaging_status_tool_uses_the_session_scoped_status_route() {
    let ts = TestServer::start().await;
    let branch = weaver_core::branch::upsert(&ts.state.db, &ts.cwd(), "weaver/mcp-status", "main")
        .await
        .unwrap();
    loom::session::insert(
        &ts.state.db,
        &loom::session::NewSession {
            id: "mcpstatussession".to_string(),
            branch_id: branch.id.clone(),
            work_dir: ts.cwd(),
            term_session: "weaver-mcp-status".to_string(),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            protocol: "acp".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
    )
    .await
    .unwrap();
    let token =
        loom::auth::create_session_token(&ts.state.db, None, "mcpstatussession", &branch.id)
            .await
            .unwrap();

    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve", "messaging"])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .env("LOOM_TOKEN", token)
        .env("LOOM_SESSION_ID", "mcpstatussession")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"status_update\",\"arguments\":{\"level\":\"attention\",\"message\":\"review the MCP design\"}}}\n",
        )
        .await
        .unwrap();
    drop(stdin);
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let _: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let response: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert!(child.wait().await.unwrap().success());

    let branch = weaver_core::branch::get(&ts.state.db, &branch.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(branch.description, "review the MCP design");
    let attention =
        weaver_core::tags::get(&ts.state.db, &branch.id, weaver_core::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(attention.value, "attention");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_tools_resolve_self_and_call_the_rest_contract() {
    let ts = TestServer::start().await;
    let branch = weaver_core::branch::upsert(&ts.state.db, &ts.cwd(), "weaver/mcp-history", "main")
        .await
        .unwrap();
    loom::session::insert(
        &ts.state.db,
        &loom::session::NewSession {
            id: "mcphistorysession".to_string(),
            branch_id: branch.id.clone(),
            work_dir: ts.cwd(),
            term_session: "weaver-mcp-history".to_string(),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            protocol: "acp".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
    )
    .await
    .unwrap();
    loom::chat::insert(
        &ts.state.db,
        "mcphistorysession",
        0,
        0,
        loom::chat::kind::AGENT_MESSAGE,
        &json!({ "text": "the searchable answer" }),
    )
    .await
    .unwrap();
    let token =
        loom::auth::create_session_token(&ts.state.db, None, "mcphistorysession", &branch.id)
            .await
            .unwrap();

    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve", "history"])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .env("LOOM_TOKEN", token)
        .env("LOOM_SESSION_ID", "mcphistorysession")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"history\",\"arguments\":{\"limit\":1}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"q\":\"SEARCHABLE\",\"kinds\":[\"message\"]}}}\n",
        )
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for id in [1, 2] {
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let page = &response["result"]["structuredContent"];
        assert_eq!(page["source"], "acp");
        assert_eq!(page["records"][0]["content"], "the searchable answer");
    }
    assert!(child.wait().await.unwrap().success());
}
