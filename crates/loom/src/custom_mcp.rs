//! Operator-authored MCP programs stored and revisioned in Loom's database.
//!
//! Definitions are admin configuration and may execute arbitrary code. They are
//! validated before use and are never admitted to restricted sessions.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use weaver_api::{CustomMcpReq, CustomMcpSnapshot, CustomMcpView};

use crate::db::{now_iso, Db};

const SOURCE_MAX_BYTES: usize = 128 * 1024;
const TEST_MAX_BYTES: usize = 128 * 1024;
const DIAGNOSTIC_MAX_BYTES: usize = 16 * 1024;
const MCP_RESPONSE_MAX_BYTES: u64 = 1024 * 1024;
const TOOL_MAX_COUNT: usize = 256;
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, FromRow)]
struct Row {
    identity: String,
    group_name: String,
    label: String,
    description: String,
    enabled: bool,
    current_revision: i64,
    source: String,
    test_source: String,
    digest: String,
    tools_json: String,
    validation_state: String,
    validation_message: String,
    created_at: String,
    updated_at: String,
}

fn row_query() -> &'static str {
    "SELECT s.identity, s.group_name, s.label, s.description, s.enabled,
            s.current_revision, r.source, r.test_source, r.digest, r.tools_json,
            r.validation_state, r.validation_message, s.created_at, s.updated_at
     FROM custom_mcp_servers s
     JOIN custom_mcp_revisions r
       ON r.identity = s.identity AND r.revision = s.current_revision"
}

fn into_view(row: Row) -> Result<CustomMcpView> {
    Ok(CustomMcpView {
        identity: row.identity,
        group: row.group_name,
        label: row.label,
        description: row.description,
        enabled: row.enabled,
        revision: row.current_revision,
        digest: row.digest,
        source: row.source,
        test_source: row.test_source,
        tools: serde_json::from_str(&row.tools_json).context("invalid custom MCP tools")?,
        validation_state: row.validation_state,
        validation_message: row.validation_message,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub fn validate_identity(identity: &str) -> Result<String> {
    let identity = identity.trim();
    if !identity.starts_with('/') || identity.len() > 160 {
        bail!("custom MCP identity must start with '/' and be at most 160 bytes");
    }
    let parts = identity[1..].split('/').collect::<Vec<_>>();
    if parts.len() < 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 64
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        bail!(
            "custom MCP identity must be /group/name[/name...] using letters, digits, '-' or '_'"
        );
    }
    Ok(parts[0].to_string())
}

fn validate_request(req: &CustomMcpReq) -> Result<String> {
    let group = validate_identity(&req.identity)?;
    if crate::mcp::is_builtin_group(&group) {
        bail!(
            "custom MCP group '{group}' is reserved by a trusted builtin; choose a distinct group"
        );
    }
    if req.label.trim().is_empty() || req.label.len() > 128 {
        bail!("custom MCP label must contain 1 to 128 bytes");
    }
    if req.description.len() > 4096 {
        bail!("custom MCP description must be at most 4096 bytes");
    }
    if req.source.trim().is_empty() || req.source.len() > SOURCE_MAX_BYTES {
        bail!("custom MCP source must contain 1 to {SOURCE_MAX_BYTES} bytes");
    }
    if req.test_source.len() > TEST_MAX_BYTES {
        bail!("custom MCP tests must be at most {TEST_MAX_BYTES} bytes");
    }
    Ok(group)
}

fn digest(source: &str, test_source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source);
    hasher.update([0]);
    hasher.update(test_source);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn bounded(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if text.len() <= DIAGNOSTIC_MAX_BYTES {
        text.to_string()
    } else {
        let mut end = DIAGNOSTIC_MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    }
}

fn uv_bin() -> String {
    std::env::var("LOOM_UV_BIN").unwrap_or_else(|_| "uv".to_string())
}

fn uv_command(cache_dir: &std::path::Path, session_context: bool) -> Command {
    let mut command = Command::new(uv_bin());
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command.env(
        "UV_CACHE_DIR",
        std::env::var_os("UV_CACHE_DIR").unwrap_or_else(|| cache_dir.into()),
    );
    command.env(
        "UV_PYTHON_INSTALL_DIR",
        std::env::var_os("UV_PYTHON_INSTALL_DIR")
            .unwrap_or_else(|| cache_dir.with_file_name(".uv-python").into()),
    );
    if session_context {
        for name in [
            "WEAVER_API",
            "WEAVER_BRANCH",
            "LOOM_SESSION_ID",
            "LOOM_TOKEN",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
    command
}

async fn run_tests(source_path: &std::path::Path, test_source: &str) -> Result<String> {
    if test_source.trim().is_empty() {
        return Ok(String::new());
    }
    let dir = source_path
        .parent()
        .context("custom MCP source has no parent")?;
    let test_path = dir.join("test_mcp.py");
    tokio::fs::write(&test_path, test_source).await?;
    let mut command = uv_command(&dir.join(".uv-cache"), false);
    let mut child = command
        .args(["run", "--script"])
        .arg(&test_path)
        .env("LOOM_MCP_SOURCE", source_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("launching custom MCP tests through uv")?;
    let stdout = child
        .stdout
        .take()
        .context("custom MCP test stdout unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("custom MCP test stderr unavailable")?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let status = match tokio::time::timeout(VALIDATION_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            stdout_task.abort();
            stderr_task.abort();
            bail!("custom MCP tests timed out");
        }
    };
    let (stdout, stderr) = tokio::try_join!(stdout_task, stderr_task)?;
    let stdout = stdout?;
    let stderr = stderr?;
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    if !status.success() {
        bail!("custom MCP tests failed:\n{}", bounded(diagnostics));
    }
    Ok(bounded(diagnostics))
}

async fn read_bounded(mut reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = DIAGNOSTIC_MAX_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(retained)
}

async fn smoke(source_path: &std::path::Path) -> Result<Vec<String>> {
    let dir = source_path
        .parent()
        .context("custom MCP source has no parent")?;
    let mut command = uv_command(&dir.join(".uv-cache"), false);
    let mut child = command
        .args(["run", "--script"])
        .arg(source_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("launching custom MCP through uv")?;
    let mut stdin = child.stdin.take().context("custom MCP stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("custom MCP stdout unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("custom MCP stderr unavailable")?;
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let result = async {
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"loom-validator\",\"version\":\"1\"}}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
            )
            .await?;
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout.take(MCP_RESPONSE_MAX_BYTES)).lines();
        let mut tools = None;
        let deadline = tokio::time::Instant::now() + VALIDATION_TIMEOUT;
        for _ in 0..8 {
            let line = tokio::time::timeout_at(deadline, lines.next_line())
                .await
                .context("custom MCP tools/list timed out")??
                .context("custom MCP closed before tools/list")?;
            let value: Value =
                serde_json::from_str(&line).context("custom MCP returned invalid JSON")?;
            if value.get("id") == Some(&json!(2)) {
                let listed = value
                    .pointer("/result/tools")
                    .and_then(Value::as_array)
                    .context("custom MCP tools/list result is missing tools")?;
                let names = listed
                    .iter()
                    .map(|tool| {
                        tool.get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .context("custom MCP tool is missing a name")
                    })
                    .collect::<Result<Vec<_>>>()?;
                if names.len() > TOOL_MAX_COUNT {
                    bail!("custom MCP may advertise at most {TOOL_MAX_COUNT} tools");
                }
                tools = Some(names);
                break;
            }
        }
        tools.context("custom MCP did not answer tools/list")
    }
    .await;
    drop(stdin);
    let _ = child.kill().await;
    let stderr = stderr_task.await??;
    let tools = result.with_context(|| {
        let stderr = bounded(String::from_utf8_lossy(&stderr));
        if stderr.is_empty() {
            "custom MCP produced no stderr diagnostics".to_string()
        } else {
            format!("custom MCP stderr:\n{stderr}")
        }
    })?;
    if tools.is_empty() {
        bail!("custom MCP must advertise at least one tool");
    }
    if tools.iter().any(|name| {
        name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) {
        bail!("custom MCP tool names must use letters, digits, '-' or '_'");
    }
    Ok(tools)
}

async fn validate_source(source: &str, test_source: &str) -> (String, Vec<String>, String) {
    let result = async {
        let dir = tempfile::tempdir().context("creating custom MCP validation directory")?;
        let source_path = dir.path().join("server.py");
        tokio::fs::write(&source_path, source).await?;
        let tools = smoke(&source_path).await?;
        let diagnostics = run_tests(&source_path, test_source).await?;
        Ok::<_, anyhow::Error>((tools, diagnostics))
    }
    .await;
    match result {
        Ok((tools, diagnostics)) => ("ready".to_string(), tools, diagnostics),
        Err(error) => (
            "failed".to_string(),
            Vec::new(),
            bounded(format!("{error:#}")),
        ),
    }
}

pub async fn list(db: &Db) -> Result<Vec<CustomMcpView>> {
    let rows = sqlx::query_as::<_, Row>(&format!("{} ORDER BY s.identity", row_query()))
        .fetch_all(db)
        .await?;
    rows.into_iter().map(into_view).collect()
}

pub async fn get(db: &Db, identity: &str) -> Result<Option<CustomMcpView>> {
    let row = sqlx::query_as::<_, Row>(&format!("{} WHERE s.identity = ?", row_query()))
        .bind(identity)
        .fetch_optional(db)
        .await?;
    row.map(into_view).transpose()
}

pub async fn upsert(db: &Db, req: &CustomMcpReq) -> Result<CustomMcpView> {
    let group = validate_request(req)?;
    let existing = get(db, &req.identity).await?;
    if let Some(existing) = &existing {
        if existing.source == req.source
            && existing.test_source == req.test_source
            && existing.label == req.label.trim()
            && existing.description == req.description.trim()
            && existing.enabled == req.enabled
        {
            return Ok(existing.clone());
        }
    }
    let revision = existing.as_ref().map_or(1, |value| value.revision + 1);
    let (validation_state, tools, validation_message) =
        validate_source(&req.source, &req.test_source).await;
    let now = now_iso();
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO custom_mcp_servers
         (identity, group_name, label, description, enabled, current_revision, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(identity) DO UPDATE SET
          group_name=excluded.group_name, label=excluded.label,
          description=excluded.description, enabled=excluded.enabled,
          current_revision=excluded.current_revision, updated_at=excluded.updated_at",
    )
    .bind(req.identity.trim())
    .bind(group)
    .bind(req.label.trim())
    .bind(req.description.trim())
    .bind(req.enabled)
    .bind(revision)
    .bind(existing.as_ref().map_or(now.as_str(), |value| value.created_at.as_str()))
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO custom_mcp_revisions
         (identity, revision, source, test_source, digest, tools_json,
          validation_state, validation_message, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(req.identity.trim())
    .bind(revision)
    .bind(&req.source)
    .bind(&req.test_source)
    .bind(digest(&req.source, &req.test_source))
    .bind(serde_json::to_string(&tools)?)
    .bind(validation_state)
    .bind(validation_message)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    get(db, req.identity.trim())
        .await?
        .ok_or_else(|| anyhow!("custom MCP vanished after upsert"))
}

pub async fn remove(db: &Db, identity: &str) -> Result<bool> {
    let Some(target) = get(db, identity).await? else {
        return Ok(false);
    };
    // Retired rows are generation tombstones, not selectable templates. Live
    // sessions carry the full executable custom-MCP source in their immutable
    // policy snapshot, so a tombstone must not pin registry administration.
    for profile in crate::profile::list(db).await? {
        if profile
            .mcp_policy_snapshot()?
            .custom_servers
            .iter()
            .any(|server| server.identity == identity)
        {
            bail!(
                "custom MCP '{}' is pinned by profile '{}'; update the profile before removing it",
                identity,
                profile.name
            );
        }
    }
    let group_has_other_server = list(db)
        .await?
        .iter()
        .any(|server| server.identity != identity && server.group == target.group);
    if !group_has_other_server {
        for profile in crate::profile::list(db).await? {
            let access = profile.mcp_access()?;
            if access.mode == "groups" && access.groups.contains(&target.group) {
                bail!(
                    "custom MCP group '{}' is selected by profile '{}'; update the profile before removing its last server",
                    target.group,
                    profile.name
                );
            }
        }
    }
    Ok(
        sqlx::query("DELETE FROM custom_mcp_servers WHERE identity = ?")
            .bind(identity)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

pub fn ready_snapshots(items: &[CustomMcpView]) -> Vec<CustomMcpSnapshot> {
    items
        .iter()
        .filter(|item| item.enabled && item.validation_state == "ready")
        .map(|item| CustomMcpSnapshot {
            server_name: server_name(&item.identity),
            identity: item.identity.clone(),
            group: item.group.clone(),
            revision: item.revision,
            digest: item.digest.clone(),
            tools: item.tools.clone(),
            source: item.source.clone(),
        })
        .collect()
}

pub fn server_name(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!("loom_custom_{}", &hex::encode(digest)[..12])
}

pub fn permission_rule(server_name: &str, tool: &str) -> String {
    format!("mcp__{server_name}__{tool}")
}

async fn forward_runtime_response(
    line: &str,
    pending_tool_lists: &mut Vec<Value>,
    client_stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<()> {
    let mut value: Value =
        serde_json::from_str(line).context("custom MCP wrote invalid JSON to stdout")?;
    if let Some(position) = value
        .get("id")
        .and_then(|id| pending_tool_lists.iter().position(|pending| pending == id))
    {
        pending_tool_lists.swap_remove(position);
        if let Some(tools) = value.pointer_mut("/result/tools") {
            *tools = crate::mcp::runtime_tools(tools.take());
        }
    }
    client_stdout
        .write_all(serde_json::to_string(&value)?.as_bytes())
        .await?;
    client_stdout.write_all(b"\n").await?;
    client_stdout.flush().await?;
    Ok(())
}

/// Decode the exact source carried in a session-stamped server config and
/// proxy stdio to its uv-managed process.
pub async fn serve_from_env(identity: &str) -> Result<()> {
    validate_identity(identity)?;
    let encoded = std::env::var("LOOM_CUSTOM_MCP_SOURCE_B64")
        .context("custom MCP source is missing from the session snapshot")?;
    let source = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decoding custom MCP source")?;
    let dir = tempfile::tempdir().context("creating custom MCP runtime directory")?;
    let source_path = dir.path().join("server.py");
    tokio::fs::write(&source_path, source).await?;
    let mut command = uv_command(&dir.path().join(".uv-cache"), true);
    let mut child = command
        .args(["run", "--script"])
        .arg(&source_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("launching custom MCP through uv")?;
    let mut child_stdin = Some(child.stdin.take().context("custom MCP stdin unavailable")?);
    let child_stdout = child
        .stdout
        .take()
        .context("custom MCP stdout unavailable")?;
    let mut client_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut server_lines = BufReader::new(child_stdout).lines();
    let mut client_stdout = tokio::io::stdout();
    let mut pending_tool_lists = Vec::<Value>::new();

    loop {
        tokio::select! {
            request = client_lines.next_line() => {
                let Some(line) = request? else {
                    let mut stdin = child_stdin
                        .take()
                        .context("custom MCP stdin closed unexpectedly")?;
                    stdin.shutdown().await?;
                    drop(stdin);
                    let drain = async {
                        while let Some(line) = server_lines.next_line().await? {
                            forward_runtime_response(
                                &line,
                                &mut pending_tool_lists,
                                &mut client_stdout,
                            )
                            .await?;
                        }
                        Ok::<_, anyhow::Error>(())
                    };
                    match tokio::time::timeout(Duration::from_secs(5), drain).await {
                        Ok(result) => result?,
                        Err(_) => {
                            child.kill().await?;
                            bail!("custom MCP '{identity}' did not exit after stdin closed");
                        }
                    }
                    break;
                };
                let value: Value = serde_json::from_str(&line)
                    .context("custom MCP proxy received invalid client JSON")?;
                if value.get("method").and_then(Value::as_str) == Some("tools/list") {
                    if let Some(id) = value.get("id") {
                        pending_tool_lists.push(id.clone());
                    }
                }
                if value.get("method").and_then(Value::as_str) == Some("tools/call") {
                    if let Some(name) = value.pointer("/params/name").and_then(Value::as_str) {
                        if !crate::mcp::runtime_tool_allowed(name) {
                            if let Some(id) = value.get("id") {
                                let response = json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {
                                        "code": -32601,
                                        "message": format!(
                                            "custom MCP tool '{name}' is not allowed by this session"
                                        )
                                    }
                                });
                                client_stdout
                                    .write_all(serde_json::to_string(&response)?.as_bytes())
                                    .await?;
                                client_stdout.write_all(b"\n").await?;
                                client_stdout.flush().await?;
                            }
                            continue;
                        }
                    }
                }
                let stdin = child_stdin
                    .as_mut()
                    .context("custom MCP stdin closed unexpectedly")?;
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
            }
            response = server_lines.next_line() => {
                let Some(line) = response? else {
                    break;
                };
                forward_runtime_response(
                    &line,
                    &mut pending_tool_lists,
                    &mut client_stdout,
                )
                .await?;
            }
        }
    }
    drop(child_stdin);
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            bail!("custom MCP '{identity}' did not exit after stdin closed");
        }
    };
    if !status.success() {
        bail!("custom MCP '{identity}' exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_absolute_grouped_paths() {
        assert_eq!(
            validate_identity("/engineering/search/docs").unwrap(),
            "engineering"
        );
        assert!(validate_identity("engineering/search").is_err());
        assert!(validate_identity("/one").is_err());
        assert!(validate_identity("/bad/../name").is_err());
    }

    #[test]
    fn server_names_and_permissions_are_stable() {
        let name = server_name("/engineering/search/docs");
        assert_eq!(name, server_name("/engineering/search/docs"));
        assert!(permission_rule(&name, "lookup").starts_with("mcp__loom_custom_"));
    }

    #[test]
    fn builtin_groups_cannot_be_shadowed() {
        let error = validate_request(&CustomMcpReq {
            identity: "/github/shadow".to_string(),
            label: "shadow".to_string(),
            source: "print('no')".to_string(),
            enabled: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("reserved by a trusted builtin"));
    }
}
