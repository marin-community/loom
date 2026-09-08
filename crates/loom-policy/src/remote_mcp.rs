//! Operator-registered remote Streamable HTTP MCP servers.

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use weaver_api::{RemoteMcpAuth, RemoteMcpReq, RemoteMcpView};

use crate::db::{now_iso, Db};
pub use crate::mcp::{get_remote as get, list_remote as list, remote_server_name as server_name};

const TOOL_MAX_COUNT: usize = 256;

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_auth(auth: &RemoteMcpAuth) -> Result<()> {
    match auth {
        RemoteMcpAuth::None => Ok(()),
        RemoteMcpAuth::Environment {
            header,
            environment,
            prefix,
        } => {
            if !valid_name(header) {
                bail!("remote MCP auth header must use letters, digits, '-' or '_'");
            }
            if !valid_name(environment) {
                bail!("remote MCP auth environment must use letters, digits, '-' or '_'");
            }
            if prefix.len() > 128 || prefix.contains(['\r', '\n', '\0']) {
                bail!("remote MCP auth prefix must be a single line of at most 128 bytes");
            }
            Ok(())
        }
        RemoteMcpAuth::Iap { audience } => {
            if audience.trim().is_empty()
                || audience.len() > 2048
                || audience.contains(['\r', '\n', '\0'])
            {
                bail!("remote MCP IAP audience must contain 1 to 2048 bytes");
            }
            Ok(())
        }
    }
}

fn normalized_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("remote MCP URL must be absolute")?;
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    if !secure && !loopback_http {
        bail!("remote MCP URL must use https, except for loopback test servers");
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("remote MCP URL cannot contain credentials or a fragment");
    }
    Ok(url.to_string())
}

fn validate_request(req: &RemoteMcpReq) -> Result<(String, String)> {
    let group = crate::custom_mcp::validate_identity(&req.identity)?;
    if crate::mcp::is_builtin_group(&group) {
        bail!("remote MCP group '{group}' is reserved by a trusted builtin");
    }
    if req.label.trim().is_empty() || req.label.len() > 128 {
        bail!("remote MCP label must contain 1 to 128 bytes");
    }
    if req.description.len() > 4096 {
        bail!("remote MCP description must be at most 4096 bytes");
    }
    validate_auth(&req.auth)?;
    if req.tools.is_empty() || req.tools.len() > TOOL_MAX_COUNT {
        bail!("remote MCP must declare 1 to {TOOL_MAX_COUNT} tools");
    }
    let mut unique = std::collections::HashSet::new();
    if req
        .tools
        .iter()
        .any(|tool| !valid_name(tool) || !unique.insert(tool))
    {
        bail!("remote MCP tool names must be unique and use letters, digits, '-' or '_'");
    }
    Ok((group, normalized_url(&req.url)?))
}

fn digest(url: &str, auth: &RemoteMcpAuth, tools: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(url);
    hasher.update([0]);
    hasher.update(serde_json::to_vec(auth)?);
    for tool in tools {
        hasher.update([0]);
        hasher.update(tool);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub async fn upsert(db: &Db, req: &RemoteMcpReq) -> Result<RemoteMcpView> {
    let (group, url) = validate_request(req)?;
    let existing = get(db, req.identity.trim()).await?;
    if existing.is_none()
        && crate::custom_mcp::get(db, req.identity.trim())
            .await?
            .is_some()
    {
        bail!(
            "MCP identity '{}' is already registered as custom",
            req.identity.trim()
        );
    }
    if let Some(existing) = &existing {
        if existing.url == url
            && existing.auth == req.auth
            && existing.tools == req.tools
            && existing.label == req.label.trim()
            && existing.description == req.description.trim()
            && existing.enabled == req.enabled
        {
            return Ok(existing.clone());
        }
    }
    let revision = existing.as_ref().map_or(1, |value| value.revision + 1);
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    sqlx::query(
        "INSERT INTO remote_mcp_servers
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
        "INSERT INTO remote_mcp_revisions
         (identity, revision, url, auth_json, digest, tools_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(req.identity.trim())
    .bind(revision)
    .bind(&url)
    .bind(serde_json::to_string(&req.auth)?)
    .bind(digest(&url, &req.auth, &req.tools)?)
    .bind(serde_json::to_string(&req.tools)?)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    get(db, req.identity.trim())
        .await?
        .ok_or_else(|| anyhow!("remote MCP vanished after upsert"))
}

pub async fn remove(db: &Db, identity: &str) -> Result<bool> {
    let Some(target) = get(db, identity).await? else {
        return Ok(false);
    };
    for profile in crate::profile::list(db).await? {
        if profile
            .mcp_policy_snapshot()?
            .remote_servers
            .iter()
            .any(|server| server.identity == identity)
        {
            bail!(
                "remote MCP '{}' is pinned by profile '{}'; update the profile before removing it",
                identity,
                profile.name
            );
        }
    }
    let group_has_other_server = list(db)
        .await?
        .iter()
        .any(|server| server.identity != identity && server.group == target.group)
        || crate::custom_mcp::list(db)
            .await?
            .iter()
            .any(|server| server.group == target.group);
    if !group_has_other_server {
        for profile in crate::profile::list(db).await? {
            let access = profile.mcp_access()?;
            if access.mode == "groups" && access.groups.contains(&target.group) {
                bail!(
                    "remote MCP group '{}' is selected by profile '{}'; update the profile before removing its last server",
                    target.group,
                    profile.name
                );
            }
        }
    }
    Ok(
        sqlx::query("DELETE FROM remote_mcp_servers WHERE identity = ?")
            .bind(identity)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

pub async fn mark_deployment_managed(db: &Db, identity: &str) -> Result<()> {
    sqlx::query("UPDATE remote_mcp_servers SET managed_by_deployment = 1 WHERE identity = ?")
        .bind(identity)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn deployment_managed_identities(db: &Db) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT identity FROM remote_mcp_servers
         WHERE managed_by_deployment = 1 ORDER BY identity",
    )
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_require_secure_transport() {
        let req = RemoteMcpReq {
            identity: "/ops/api".to_string(),
            label: "API".to_string(),
            url: "http://example.com/mcp".to_string(),
            tools: vec!["read".to_string()],
            ..Default::default()
        };
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn iap_auth_has_an_explicit_wire_variant() {
        let req: RemoteMcpReq = serde_json::from_value(serde_json::json!({
            "identity": "/ops/api",
            "label": "API",
            "url": "https://example.com/mcp",
            "auth": {"type": "iap", "audience": "iap-client-id"},
            "tools": ["read"]
        }))
        .unwrap();

        assert_eq!(
            req.auth,
            RemoteMcpAuth::Iap {
                audience: "iap-client-id".to_string()
            }
        );
        validate_request(&req).unwrap();
    }
}
