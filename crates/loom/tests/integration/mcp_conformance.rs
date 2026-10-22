//! Real-process conformance for Loom's aggregate built-in MCP server.

use std::process::Stdio;

use base64::Engine as _;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

use crate::fixtures::TestServer;
use weaver_api::operations::sessions;

fn loom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loom")
}

async fn run_builtin() -> (Vec<Value>, std::process::ExitStatus) {
    // Its own home, so this cannot depend on which test ran last: the adapter
    // is a subprocess and inherits whatever `WEAVER_HOME` the process-global
    // env happens to hold.
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(loom_bin())
        .env("WEAVER_HOME", home.path())
        .args(["mcp", "serve"])
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

async fn run_filtered_builtin(tools: &[&str]) -> Vec<Value> {
    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve"])
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
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"github_issue_edit\",\"arguments\":{}}}\n",
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

async fn run_scoped_calls(ts: &TestServer, token: &str, calls: &[Value]) -> Vec<Value> {
    let mut child = Command::new(loom_bin())
        .args(["mcp", "serve"])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .env("LOOM_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for call in calls {
        stdin
            .write_all(format!("{call}\n").as_bytes())
            .await
            .unwrap();
    }
    drop(stdin);
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut values = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        values.push(serde_json::from_str(&line).unwrap());
    }
    assert!(child.wait().await.unwrap().success());
    values
}

/// `#[serial]` because the adapter subprocesses inherit process-global env
/// (`WEAVER_HOME`, `WEAVER_API`) that every `TestServer::start` rewrites. Run
/// concurrently, this test points its adapters at another test's home while
/// that home is being torn down, and fails on whatever error comes back.
#[serial]
#[tokio::test]
async fn builtin_server_speaks_mcp_stdio() {
    let (values, status) = run_builtin().await;
    assert!(status.success());
    assert_eq!(values.len(), 4, "server replied to a notification");
    assert_eq!(values[0]["id"], 1);
    assert_eq!(values[0]["result"]["serverInfo"]["name"], "loom");
    assert_eq!(values[1]["id"], 2);
    let tools = values[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 46);
    assert!(tools.iter().any(|tool| tool["name"] == "channel_send"));
    assert!(tools.iter().any(|tool| tool["name"] == "session_history"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "messaging_slack_send"));
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
        .unwrap_or_default()
        .contains("unknown"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_issue_and_permission_projections_invoke_the_rest_contract() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("typed MCP projections".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let issue = ts
        .client
        .invoke::<weaver_api::operations::issues::create::Op>(
            &weaver_api::operations::issues::create::Input {
                title: "close through MCP".to_string(),
                branch: created.branch.id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let token =
        loom::auth::create_session_token(&ts.state.db, None, &created.id, &created.branch.id)
            .await
            .unwrap();

    let issue_responses = run_scoped_calls(
        &ts,
        &token,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "issue_get",
                    "arguments": { "id": issue.id, "schema_invisible": "ignored" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                // `issues.close` applies atomically to a set; the single-id case is
                // the one-element set.
                "params": { "name": "issue_close", "arguments": { "ids": [issue.id] } }
            }),
        ],
    )
    .await;
    assert_eq!(issue_responses.len(), 2);
    assert_eq!(issue_responses[0]["result"]["isError"], false);
    assert_eq!(
        issue_responses[0]["result"]["structuredContent"]["id"],
        issue.id
    );
    assert_eq!(issue_responses[1]["result"]["isError"], false);
    assert_eq!(
        issue_responses[1]["result"]["structuredContent"]["issues"][0]["status"],
        "closed"
    );

    let permission_responses = run_scoped_calls(
        &ts,
        &token,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "permission_show", "arguments": {} }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "permission_explain",
                    "arguments": { "operation": "issues.close" }
                }
            }),
        ],
    )
    .await;
    assert_eq!(permission_responses.len(), 2);
    assert_eq!(
        permission_responses[0]["result"]["structuredContent"]["session_id"],
        created.id
    );
    assert_eq!(
        permission_responses[1]["result"]["structuredContent"]["id"],
        "issues.close"
    );

    ts.client
        .post("/api/sessions/delete", json!({ "session": created.id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn malformed_json_fails_closed_and_clean_eof_succeeds() {
    let clean = Command::new(loom_bin())
        .args(["mcp", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(clean.success());

    let mut malformed = Command::new(loom_bin())
        .args(["mcp", "serve"])
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
    let values = run_filtered_builtin(&["github_issue_view"]).await;
    assert_eq!(
        values[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["github_issue_view"]
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
async fn session_status_tool_uses_the_session_scoped_status_route() {
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
        .args(["mcp", "serve"])
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
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"session_status_set\",\"arguments\":{\"level\":\"attention\",\"message\":\"review the MCP design\"}}}\n",
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
        .args(["mcp", "serve"])
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
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"session_history\",\"arguments\":{\"limit\":1}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"session_search\",\"arguments\":{\"q\":\"SEARCHABLE\",\"kinds\":[\"message\"]}}}\n",
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

// ---------------------------------------------------------------------------
// The remote endpoint: the same protocol over Streamable HTTP at `/mcp`, with
// credentials from Loom's OAuth authorization server.
// ---------------------------------------------------------------------------

const CALLBACK: &str = "http://127.0.0.1:9/callback";

fn remote_url(ts: &TestServer, path: &str) -> String {
    format!("http://{}{}", ts.addr, path)
}

/// A client that shows redirects instead of following them, so the OAuth
/// hops can be inspected.
fn remote_http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn enable_connector(ts: &TestServer) {
    ts.client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "connector.enabled": "true" } }),
        )
        .await
        .unwrap();
}

async fn rpc(ts: &TestServer, token: &str, message: Value) -> reqwest::Response {
    remote_http()
        .post(remote_url(ts, "/mcp"))
        .bearer_auth(token)
        .header("Accept", "application/json, text/event-stream")
        .json(&message)
        .send()
        .await
        .unwrap()
}

async fn call(ts: &TestServer, token: &str, tool: &str, arguments: Value) -> Value {
    let response = rpc(
        ts,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }),
    )
    .await;
    assert_eq!(response.status(), 200);
    response.json::<Value>().await.unwrap()["result"].clone()
}

fn pkce_pair() -> (String, String) {
    use sha2::Digest;
    let verifier = "remote-endpoint-verifier-0123456789-abcdefghijklmnop".to_string();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn query_value(location: &str, key: &str) -> Option<String> {
    reqwest::Url::parse(location)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

/// Register a client, sign `username` in through consent, and exchange the
/// code. Returns the client id and secret with the token response.
async fn connect(ts: &TestServer, username: &str) -> (String, String, Value) {
    let created = ts
        .client
        .invoke::<weaver_api::operations::auth::oauth_clients::create::Op>(
            &weaver_api::operations::auth::oauth_clients::create::Input {
                name: "Test <assistant>".to_string(),
                redirect_uris: vec![CALLBACK.to_string()],
                public: false,
            },
        )
        .await
        .unwrap();
    let client_id = created.client.client_id.clone();
    let secret = created.client_secret.unwrap();
    let http = remote_http();
    let (verifier, challenge) = pkce_pair();
    let authorize = reqwest::Url::parse_with_params(
        &remote_url(ts, "/oauth/authorize"),
        &[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", CALLBACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", "opaque-state"),
            ("resource", &remote_url(ts, "/mcp")),
        ],
    )
    .unwrap();

    // Not signed in: the person goes through Loom's sign-in and comes back.
    let anonymous = http.get(authorize.clone()).send().await.unwrap();
    assert_eq!(anonymous.status(), 303);
    let login = anonymous.headers()["location"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        login.starts_with("/login?redirect=%2Foauth%2Fauthorize"),
        "{login}"
    );

    let cookie = loom::auth::create_session(&ts.state.db, username)
        .await
        .unwrap();
    let consent = http
        .get(authorize)
        .header("Cookie", format!("loom_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(consent.status(), 200);
    assert_eq!(consent.headers()["x-frame-options"], "DENY");
    let consent_cookie = consent.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let html = consent.text().await.unwrap();
    assert!(html.contains("Test &lt;assistant&gt;"), "{html}");
    assert!(html.contains(username));
    let csrf = consent_cookie.split_once('=').unwrap().1.to_string();

    let approved = http
        .post(remote_url(ts, "/oauth/authorize"))
        .header("Cookie", format!("loom_session={cookie}; {consent_cookie}"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", CALLBACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", "opaque-state"),
            ("csrf", csrf.as_str()),
            ("decision", "approve"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), 303);
    let location = approved.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with(CALLBACK), "{location}");
    assert_eq!(
        query_value(&location, "state").as_deref(),
        Some("opaque-state")
    );
    assert_eq!(
        query_value(&location, "iss"),
        Some(format!("http://{}", ts.addr))
    );
    let code = query_value(&location, "code").unwrap();

    let exchange = |code: String| {
        http.post(remote_url(ts, "/oauth/token"))
            .basic_auth(&client_id, Some(&secret))
            .form(&[
                ("grant_type", "authorization_code".to_string()),
                ("code", code),
                ("redirect_uri", CALLBACK.to_string()),
                ("code_verifier", verifier.clone()),
            ])
            .send()
    };
    let tokens = exchange(code.clone()).await.unwrap();
    assert_eq!(tokens.status(), 200);
    assert!(tokens.headers()["cache-control"]
        .to_str()
        .unwrap()
        .starts_with("no-store"));
    let tokens: Value = tokens.json().await.unwrap();
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);

    let replayed = exchange(code).await.unwrap();
    assert_eq!(replayed.status(), 400);
    assert_eq!(
        replayed.json::<Value>().await.unwrap()["error"],
        "invalid_grant"
    );
    (client_id, secret, tokens)
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_endpoint_is_off_by_default_and_points_clients_at_oauth() {
    let ts = TestServer::start_api_only().await;
    let http = remote_http();
    let base = format!("http://{}", ts.addr);

    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let response = http.get(remote_url(&ts, path)).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    let disabled = http
        .post(remote_url(&ts, "/mcp"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), 404);

    enable_connector(&ts).await;
    let challenge = http
        .post(remote_url(&ts, "/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(challenge.status(), 401);
    let metadata_url = format!("{base}/.well-known/oauth-protected-resource/mcp");
    assert_eq!(
        challenge.headers()["www-authenticate"],
        format!("Bearer resource_metadata=\"{metadata_url}\"").as_str()
    );

    let resource: Value = http
        .get(&metadata_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resource["resource"], format!("{base}/mcp"));
    assert_eq!(resource["authorization_servers"], json!([base]));

    let server: Value = http
        .get(remote_url(&ts, "/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(server["issuer"], base);
    assert_eq!(server["token_endpoint"], format!("{base}/oauth/token"));
    assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));

    let invalid = rpc(&ts, "loom_not-a-token", json!({})).await;
    assert_eq!(invalid.status(), 401);
    assert!(invalid.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("error=\"invalid_token\""));

    let no_stream = http.get(remote_url(&ts, "/mcp")).send().await.unwrap();
    assert_eq!(no_stream.status(), 405);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_endpoint_speaks_mcp_over_http_with_an_oauth_credential() {
    let ts = TestServer::start().await;
    enable_connector(&ts).await;
    loom::auth::add_user(
        &ts.state.db,
        "ada",
        None,
        None,
        None,
        loom::auth::UserRole::User,
    )
    .await
    .unwrap();
    let (client_id, secret, tokens) = connect(&ts, "ada").await;
    let token = tokens["access_token"].as_str().unwrap().to_string();

    let initialized = rpc(
        &ts,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {} }
        }),
    )
    .await;
    assert_eq!(initialized.status(), 200);
    let initialized: Value = initialized.json().await.unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "loom");
    assert!(initialized["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("launch"));

    let notified = rpc(
        &ts,
        &token,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(notified.status(), 202);

    let listed: Value = rpc(
        &ts,
        &token,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await
    .json()
    .await
    .unwrap();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
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

    let unknown: Value = rpc(
        &ts,
        &token,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "unknown/method" }),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32601);

    // A session someone else started; the person reaches it by naming it.
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: Some("remote MCP projections".to_string()),
            cwd: ts.cwd(),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let session = json!({ "session": created.id });

    let got = call(&ts, &token, "get", session.clone()).await;
    assert_eq!(got["isError"], false, "{got}");
    assert_eq!(got["structuredContent"]["id"], created.id);
    let status = call(&ts, &token, "status", session.clone()).await;
    assert_eq!(status["isError"], false, "{status}");
    let url = call(&ts, &token, "url", session.clone()).await;
    assert_eq!(url["isError"], false, "{url}");
    assert!(url["structuredContent"]["url"]
        .as_str()
        .unwrap()
        .contains(&created.id));
    let messages = call(
        &ts,
        &token,
        "messages",
        json!({ "channel": created.id, "kinds": ["result"], "peek": true }),
    )
    .await;
    assert_eq!(messages["isError"], false, "{messages}");
    let listed = call(&ts, &token, "list", json!({})).await;
    assert_eq!(listed["isError"], false, "{listed}");

    let unnamed = call(&ts, &token, "get", json!({})).await;
    assert_eq!(unnamed["isError"], true);
    assert!(unnamed["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("missing required argument(s): session"));
    let smuggled = call(
        &ts,
        &token,
        "launch",
        json!({ "goal": "x", "parent_branch": created.branch.id }),
    )
    .await;
    assert_eq!(smuggled["isError"], true);
    assert!(smuggled["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("does not accept argument(s): parent_branch"));

    // Refresh rotates the pair; the old access token stops working.
    let http = remote_http();
    let refreshed = http
        .post(remote_url(&ts, "/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", tokens["refresh_token"].as_str().unwrap()),
            ("client_id", client_id.as_str()),
            ("client_secret", secret.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(refreshed.status(), 200);
    let refreshed: Value = refreshed.json().await.unwrap();
    let rotated = refreshed["access_token"].as_str().unwrap().to_string();
    let stale = rpc(
        &ts,
        &token,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
    )
    .await;
    assert_eq!(stale.status(), 401);

    // An agent's session credential has its own server; this door is for people.
    let session_token =
        loom::auth::create_session_token(&ts.state.db, None, &created.id, &created.branch.id)
            .await
            .unwrap();
    let refused = rpc(
        &ts,
        &session_token,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }),
    )
    .await;
    assert_eq!(refused.status(), 403);

    // Removing the client revokes what it was issued.
    ts.client
        .invoke::<weaver_api::operations::auth::oauth_clients::remove::Op>(
            &weaver_api::operations::auth::oauth_clients::remove::Input {
                client_id: client_id.clone(),
            },
        )
        .await
        .unwrap();
    let revoked = rpc(
        &ts,
        &rotated,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/list" }),
    )
    .await;
    assert_eq!(revoked.status(), 401);

    ts.client
        .post("/api/sessions/delete", json!({ "session": created.id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_defaults_to_the_connector_profile() {
    let ts = TestServer::start_api_only().await;
    enable_connector(&ts).await;
    ts.client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "connector.default_profile": "no-such-connector-profile" } }),
        )
        .await
        .unwrap();
    let token = loom::auth::create_token(&ts.state.db, "rjpower", "remote test", None)
        .await
        .unwrap()
        .0;
    let launched = call(
        &ts,
        &token,
        "launch",
        json!({ "goal": "tidy the README", "repo": "octo/remote-test" }),
    )
    .await;
    assert_eq!(launched["isError"], true, "{launched}");
    assert!(
        launched["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no-such-connector-profile"),
        "{launched}"
    );
}
