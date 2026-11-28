//! The `codex` builtin: Codex CLI over its terminal command or the
//! `codex-acp` adapter, with Loom's approval policy mapped onto its env
//! contract (`CODEX_CONFIG`, `INITIAL_AGENT_MODE`, `DEFAULT_AUTH_REQUEST`).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};
use weaver_core::BoxFut;

use super::{
    binary_on_path, effort_flag, join_args, npm_adapter_cmd, spaced, AcpContract, BuiltinHarness,
};
use crate::agent::{
    sh_single_quote_path, start_terminal, AgentChoice, AgentLaunchContext, AgentMetadata, AgentType,
};
use crate::db::Db;
use loom_ctx::agent_kind::BuiltinAgentKind;

pub static HARNESS: CodexAgentType = CodexAgentType;

const MODEL_CHOICES: &[(&str, &str)] = &[
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4 Mini"),
    ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
];

const EFFORT_CHOICES: &[(&str, &str)] = &[
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "X-High"),
];

/// Codex's workspace-write mode, with approvals owned by Loom.
pub const CODEX_AGENT_MODE: &str = "agent";

pub struct CodexAgentType;

impl AgentType for CodexAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "codex".to_string(),
            label: "Codex".to_string(),
            models: AgentChoice::list(MODEL_CHOICES),
            efforts: AgentChoice::list(EFFORT_CHOICES),
            accepts_raw_model: false,
            supports_hooks: false,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
            modes: self.acp_modes().into_iter().map(str::to_string).collect(),
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }
}

impl BuiltinHarness for CodexAgentType {
    fn kind(&self) -> BuiltinAgentKind {
        BuiltinAgentKind::Codex
    }

    fn credential_names(&self) -> &'static [&'static str] {
        &["OPENAI_API_KEY"]
    }

    fn available(&self) -> BoxFut<'static, bool> {
        Box::pin(binary_on_path("codex"))
    }

    fn acp_command(&self, db: Db) -> BoxFut<'static, String> {
        Box::pin(async move {
            // `WEAVER_CODEX_ACP_CMD` (env) wins, then the `acp.codex_cmd`
            // setting, else the npm default (the package bundles a compatible
            // `@openai/codex`).
            if let Some(cmd) =
                super::acp_cmd_override(&db, "WEAVER_CODEX_ACP_CMD", "acp.codex_cmd").await
            {
                return cmd;
            }
            npm_adapter_cmd("codex-acp", "@agentclientprotocol/codex-acp")
        })
    }

    fn acp_mode_for(&self, mode: &str) -> Option<&'static str> {
        let mode = mode.trim();
        Some(match mode {
            "bypassPermissions" => "agent-full-access",
            // codex-acp has no distinct auto-approve mode. `agent` supplies the
            // workspace sandbox; Loom answers its one-shot approval requests.
            "acceptEdits" | "default" | "auto" | "" => CODEX_AGENT_MODE,
            "plan" => "read-only",
            // Codex's own mode ids pass through so an operator can name one
            // directly — `agent` is the id a live session persists for its
            // ordinary posture.
            "agent" => CODEX_AGENT_MODE,
            "read-only" => "read-only",
            "agent-full-access" => "agent-full-access",
            _ => return None,
        })
    }

    fn seeds_primer_as_goal(&self) -> bool {
        true
    }

    fn boots_in_acp_mode(&self) -> bool {
        true
    }

    fn prepare_acp_env(
        &self,
        env: &mut Vec<(String, String)>,
        contract: &AcpContract<'_>,
    ) -> Result<Vec<String>> {
        super::push_env_default(env, "DEFAULT_AUTH_REQUEST", r#"{"methodId":"api-key"}"#);
        let codex_mode = if contract.mode_from_live {
            contract.mode.trim().to_string()
        } else {
            self.launch_mode_id(contract.mode)?
        };
        let additional_directories =
            configure_acp_env(env, contract.model, contract.effort, &codex_mode)?;
        super::push_env_default(env, "INITIAL_AGENT_MODE", &codex_mode);
        Ok(additional_directories)
    }
}

impl CodexAgentType {
    /// Map a Loom posture onto a `codex-acp` `INITIAL_AGENT_MODE` id. Codex's
    /// own ids pass through, so an operator can name `read-only`/`agent`/
    /// `agent-full-access` directly. An id codex has no equivalent for is
    /// rejected rather than silently booting in `agent` — a different
    /// permission posture than the caller asked for.
    fn launch_mode_id(&self, mode: &str) -> Result<String> {
        match self.acp_mode_for(mode) {
            Some(mapped) => Ok(mapped.to_string()),
            None => Err(anyhow!(
                "codex has no '{mode}' mode (modes: {})",
                self.acp_modes().join(", ")
            )),
        }
    }
}

/// Apply Loom's Codex policy on top of the shared provider login and any
/// operator-supplied adapter config.
///
/// `agent` mode's on-request approval policy needs Loom in the loop to apply
/// its deterministic one-shot decisions, so the default reviewer is `user`
/// unless `CODEX_CONFIG` already names one. Weaver commands still need to
/// reach Loom from inside that sandbox, so `agent` mode also enables Codex's
/// network proxy for the control plane and, when GitHub auth is enabled, GitHub.
fn configure_acp_env(
    env: &mut Vec<(String, String)>,
    model: &str,
    effort: &str,
    codex_mode: &str,
) -> Result<Vec<String>> {
    let mut config = match env
        .iter()
        .rev()
        .find_map(|(name, value)| (name == "CODEX_CONFIG").then_some(value))
    {
        Some(value) => serde_json::from_str::<Value>(value).context("invalid CODEX_CONFIG JSON")?,
        None => Value::Object(default_config(model, effort)),
    };
    let config = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("CODEX_CONFIG must be a JSON object"))?;
    if codex_mode.trim() == CODEX_AGENT_MODE {
        config
            .entry("approvals_reviewer".to_string())
            .or_insert_with(|| json!("user"));

        let sandbox = config
            .entry("sandbox_workspace_write".to_string())
            .or_insert_with(|| json!({}));
        let sandbox = sandbox
            .as_object_mut()
            .ok_or_else(|| anyhow!("CODEX_CONFIG.sandbox_workspace_write must be a JSON object"))?;
        sandbox.insert("network_access".to_string(), json!(true));
    }
    let features = config
        .entry("features".to_string())
        .or_insert_with(|| json!({}));
    let features = features
        .as_object_mut()
        .ok_or_else(|| anyhow!("CODEX_CONFIG.features must be a JSON object"))?;
    features.insert("apps".to_string(), json!(false));
    if codex_mode.trim() == CODEX_AGENT_MODE {
        let proxy = features
            .entry("network_proxy".to_string())
            .or_insert_with(|| json!({}));
        if proxy.is_boolean() {
            *proxy = json!({});
        }
        let proxy = proxy
            .as_object_mut()
            .ok_or_else(|| anyhow!("CODEX_CONFIG.features.network_proxy must be a JSON object"))?;
        proxy.insert("enabled".to_string(), json!(true));
        // The Docker control-plane alias resolves to a private address. Domain
        // allow rules alone do not bypass Codex's separate private-network gate.
        proxy.insert("allow_local_binding".to_string(), json!(true));
        let api_host = env
            .iter()
            .rev()
            .find_map(|(name, value)| (name == "WEAVER_API").then_some(value))
            .map(|value| reqwest::Url::parse(value))
            .transpose()
            .context("invalid session WEAVER_API URL")?
            .and_then(|url| url.host_str().map(str::to_string));
        let domains = proxy
            .entry("domains".to_string())
            .or_insert_with(|| json!({}));
        let domains = domains.as_object_mut().ok_or_else(|| {
            anyhow!("CODEX_CONFIG.features.network_proxy.domains must be a JSON object")
        })?;
        for host in ["127.0.0.1", "localhost"]
            .into_iter()
            .chain(api_host.as_deref())
        {
            domains.insert(host.to_string(), json!("allow"));
        }
        if env.iter().any(|(name, value)| {
            name == "LOOM_GITHUB_AUTH_MODE" && matches!(value.as_str(), "broker" | "direct")
        }) {
            // Credential brokerage and GitHub transport need separate network
            // access. Preserve an operator's explicit GitHub deny rule.
            for host in ["github.com", "api.github.com"] {
                domains.entry(host.to_string()).or_insert(json!("allow"));
            }
        }
    }

    // The adapter supplies its own sandbox policy on every turn. Configured
    // writable roots must also travel in the ACP session's additionalDirectories.
    let mut additional_directories: Vec<String> = config
        .get("sandbox_workspace_write")
        .and_then(|sandbox| sandbox.get("writable_roots"))
        .map(|roots| serde_json::from_value(roots.clone()))
        .transpose()
        .context("CODEX_CONFIG.sandbox_workspace_write.writable_roots must be an array of paths")?
        .unwrap_or_default();
    anyhow::ensure!(
        additional_directories
            .iter()
            .all(|root| Path::new(root).is_absolute()),
        "CODEX_CONFIG.sandbox_workspace_write.writable_roots must contain absolute paths"
    );
    let mut seen = HashSet::new();
    additional_directories.retain(|root| seen.insert(root.clone()));

    env.retain(|(name, _)| name != "CODEX_CONFIG");
    env.push((
        "CODEX_CONFIG".to_string(),
        Value::Object(config.clone()).to_string(),
    ));
    Ok(additional_directories)
}

fn default_config(model: &str, effort: &str) -> Map<String, Value> {
    let mut cfg = Map::new();
    let model = model.trim();
    if !model.is_empty() {
        cfg.insert("model".to_string(), json!(model));
    }
    if let Some(e) = effort_flag(effort) {
        cfg.insert("model_reasoning_effort".to_string(), json!(e));
    }
    cfg
}

// -- terminal launch --------------------------------------------------------

pub(crate) fn codex_command(ctx: &AgentLaunchContext<'_>) -> String {
    let args = spaced(join_args([
        codex_model_arg(ctx.model),
        codex_effort_arg(ctx.effort),
    ]));
    match ctx.goal_file.or(ctx.primer_file) {
        Some(f) => format!(
            "codex --disable apps{args} \"$(cat {})\"",
            sh_single_quote_path(f)
        ),
        None => format!("codex --disable apps{args}"),
    }
}

fn codex_model_arg(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some(format!("--model {model}"))
}

fn codex_effort_arg(effort: &str) -> Option<String> {
    super::effort_flag(effort).map(|e| format!("-c model_reasoning_effort=\\\"{e}\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_ctx::agent_kind::DEFAULT_ACP_MODE;

    fn codex_environment_config(env: &[(String, String)]) -> Value {
        let config = env
            .iter()
            .find_map(|(name, value)| (name == "CODEX_CONFIG").then_some(value))
            .unwrap();
        serde_json::from_str(config).unwrap()
    }

    #[test]
    fn codex_acp_config_carries_only_configured_fields() {
        assert!(default_config("", "").is_empty());

        let cfg = default_config("gpt-5.3-codex", "high");
        assert_eq!(cfg["model"], "gpt-5.3-codex");
        assert_eq!(cfg["model_reasoning_effort"], "high");

        let model_only = default_config("gpt-5.3-codex", " ");
        assert_eq!(model_only["model"], "gpt-5.3-codex");
        assert!(model_only.get("model_reasoning_effort").is_none());
    }

    #[test]
    fn codex_acp_agent_mode_routes_approvals_to_loom() {
        let mut env = vec![
            (
                "CODEX_CONFIG".to_string(),
                r#"{"model":"operator","features":{"shell_snapshot":true,"apps":true}}"#
                    .to_string(),
            ),
            (
                "WEAVER_API".to_string(),
                "http://control-plane:4321".to_string(),
            ),
        ];

        configure_acp_env(&mut env, "ignored", "high", "agent").unwrap();

        let config = codex_environment_config(&env);
        assert_eq!(config["model"], "operator");
        assert_eq!(config["approvals_reviewer"], "user");
        assert_eq!(config["sandbox_workspace_write"]["network_access"], true);
        assert_eq!(config["features"]["shell_snapshot"], true);
        assert_eq!(config["features"]["apps"], false);
        assert_eq!(config["features"]["network_proxy"]["enabled"], true);
        assert_eq!(
            config["features"]["network_proxy"]["allow_local_binding"],
            true
        );
        for host in ["127.0.0.1", "localhost", "control-plane"] {
            assert_eq!(
                config["features"]["network_proxy"]["domains"][host],
                "allow"
            );
        }
        assert!(config["features"]["network_proxy"]["domains"]
            .get("*")
            .is_none());
    }

    #[test]
    fn codex_acp_github_network_access_follows_the_credential_policy() {
        for mode in ["broker", "direct", "disabled", ""] {
            let mut env = vec![("LOOM_GITHUB_AUTH_MODE".to_string(), mode.to_string())];
            configure_acp_env(&mut env, "", "", "agent").unwrap();
            let config = codex_environment_config(&env);
            let domains = &config["features"]["network_proxy"]["domains"];
            for host in ["github.com", "api.github.com"] {
                if matches!(mode, "broker" | "direct") {
                    assert_eq!(domains[host], "allow", "{mode}: {host}");
                } else {
                    assert!(domains.get(host).is_none(), "{mode}: {host}");
                }
            }
            assert!(domains.get("*").is_none());
        }

        let mut env = vec![
            ("LOOM_GITHUB_AUTH_MODE".to_string(), "broker".to_string()),
            (
                "CODEX_CONFIG".to_string(),
                json!({"features": {"network_proxy": {"domains": {
                    "api.github.com": "deny", "packages.example.com": "allow"
                }}}})
                .to_string(),
            ),
        ];
        configure_acp_env(&mut env, "", "", "agent").unwrap();
        let config = codex_environment_config(&env);
        let domains = &config["features"]["network_proxy"]["domains"];
        assert_eq!(domains["api.github.com"], "deny");
        assert_eq!(domains["packages.example.com"], "allow");
    }

    #[test]
    fn codex_acp_rejects_invalid_writable_roots() {
        for roots in [
            serde_json::json!("/cache"),
            serde_json::json!([1]),
            serde_json::json!(["relative/cache"]),
            serde_json::json!([""]),
            Value::Null,
        ] {
            let mut env = vec![(
                "CODEX_CONFIG".to_string(),
                serde_json::json!({"sandbox_workspace_write": {"writable_roots": roots}})
                    .to_string(),
            )];
            let error = configure_acp_env(&mut env, "", "", "agent").unwrap_err();
            assert!(error.to_string().contains("writable_roots"), "{error:#}");
        }
    }

    #[test]
    fn codex_acp_configuration_preserves_explicit_reviewer() {
        let mut explicit_reviewer = vec![(
            "CODEX_CONFIG".to_string(),
            r#"{"approvals_reviewer":"auto_review"}"#.to_string(),
        )];
        configure_acp_env(&mut explicit_reviewer, "", "", "agent").unwrap();
        let config: Value = serde_json::from_str(&explicit_reviewer[0].1).unwrap();
        assert_eq!(config["approvals_reviewer"], "auto_review");
    }

    #[test]
    fn codex_acp_only_agent_mode_sets_a_default_reviewer() {
        for mode in ["read-only", "agent-full-access"] {
            let mut env = Vec::new();
            configure_acp_env(&mut env, "", "", mode).unwrap();
            let config: Value = serde_json::from_str(&env[0].1).unwrap();
            assert!(
                config.get("approvals_reviewer").is_none(),
                "{mode} must not set a default reviewer"
            );
            assert!(
                config.get("sandbox_workspace_write").is_none(),
                "{mode} must not change sandbox networking"
            );
            assert!(
                config["features"].get("network_proxy").is_none(),
                "{mode} must not start the sandbox network proxy"
            );
        }
    }

    #[test]
    fn sessions_boot_in_auto_by_default() {
        // Every ACP session boots in the provider-neutral `auto` posture unless
        // overridden. Codex uses its workspace-write `agent` sandbox mode while
        // Loom owns its one-shot approval decisions.
        assert_eq!(DEFAULT_ACP_MODE, "auto");
        assert_eq!(HARNESS.launch_mode_id(DEFAULT_ACP_MODE).unwrap(), "agent");
    }

    #[test]
    fn codex_acp_mode_maps_the_claude_vocabulary_and_passes_codex_ids_through() {
        assert_eq!(
            HARNESS.acp_mode_for("bypassPermissions"),
            Some("agent-full-access")
        );
        assert_eq!(
            HARNESS.acp_mode_for(" acceptEdits "),
            Some(CODEX_AGENT_MODE)
        );
        assert_eq!(HARNESS.acp_mode_for("plan"), Some("read-only"));
        assert_eq!(HARNESS.acp_mode_for("read-only"), Some("read-only"));
        assert_eq!(
            HARNESS.acp_mode_for(CODEX_AGENT_MODE),
            Some(CODEX_AGENT_MODE)
        );
        assert_eq!(
            HARNESS.acp_mode_for("agent-full-access"),
            Some("agent-full-access")
        );
        assert_eq!(HARNESS.acp_mode_for("not-a-mode"), None);
    }

    #[test]
    fn launch_mode_id_rejects_an_unknown_posture() {
        assert_eq!(HARNESS.launch_mode_id("plan").unwrap(), "read-only");
        let err = HARNESS
            .launch_mode_id("not-a-mode")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("codex has no 'not-a-mode' mode"), "{err}");
    }

    #[test]
    fn a_configured_sandbox_yields_additional_directories() {
        let mut env = vec![(
            "CODEX_CONFIG".to_string(),
            r#"{"sandbox_workspace_write":{"writable_roots":["/a","/b","/a"]}}"#.to_string(),
        )];
        let dirs = configure_acp_env(&mut env, "gpt-6-sol", "high", "plan").unwrap();
        assert_eq!(dirs, vec!["/a".to_string(), "/b".to_string()]);
    }
}
