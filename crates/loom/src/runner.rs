//! Placement of long-lived Tapestry supervisors.
//!
//! Transport remains the shared Unix socket and relay spool under
//! `WEAVER_TAPESTRY_DIR`. A runner only decides where the supervisor process
//! lives. Production uses a Docker container per supervisor so replacing the
//! Loom control-plane container does not kill live agents; local development
//! keeps the detached-process behavior.

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const SESSION_LABEL: &str = "dev.loom.runner=session";
const SESSION_CONTAINER_PREFIX: &str = "loom-session-";
const CONTAINER_HOME: &str = "/home/app";
const CONTAINER_WEAVER_HOME: &str = "/home/app/.weaver";
const CONTAINER_SOCKET_DIR: &str = "/home/app/.weaver/sock";
const CONTAINER_SESSION_SLUG_LENGTH: usize = 48;
const CONTAINER_SESSION_HASH_LENGTH: usize = 12;

static RUNNER_CONFIG: OnceLock<std::result::Result<RunnerConfig, String>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunnerConfig {
    Local,
    Docker(DockerConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DockerConfig {
    image: String,
    home_volume: String,
    uv_volume: String,
    docker_gid: String,
    network: String,
    api_url: String,
}

impl RunnerConfig {
    fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let kind = value("LOOM_RUNNER").unwrap_or_else(|| "local".to_string());
        match kind.as_str() {
            "local" => Ok(Self::Local),
            "docker" => Ok(Self::Docker(DockerConfig {
                image: required_value(&mut value, "LOOM_SESSION_IMAGE")?,
                home_volume: required_value(&mut value, "LOOM_SESSION_HOME_VOLUME")?,
                uv_volume: required_value(&mut value, "LOOM_SESSION_UV_VOLUME")?,
                docker_gid: docker_gid(&mut value)?,
                network: required_value(&mut value, "LOOM_SESSION_NETWORK")?,
                api_url: required_value(&mut value, "LOOM_SESSION_API_URL")?,
            })),
            other => bail!("unknown LOOM_RUNNER {other:?}; expected local or docker"),
        }
    }
}

fn docker_gid(value: &mut impl FnMut(&str) -> Option<String>) -> Result<String> {
    let gid = required_value(value, "LOOM_SESSION_DOCKER_GID")?;
    if !gid.chars().all(|character| character.is_ascii_digit()) {
        bail!("LOOM_SESSION_DOCKER_GID must be numeric");
    }
    Ok(gid)
}

fn required_value(value: &mut impl FnMut(&str) -> Option<String>, name: &str) -> Result<String> {
    let resolved = value(name).unwrap_or_default();
    if resolved.is_empty() {
        bail!("{name} is required when LOOM_RUNNER=docker");
    }
    Ok(resolved)
}

fn config() -> Result<&'static RunnerConfig> {
    match RUNNER_CONFIG.get_or_init(|| {
        RunnerConfig::from_values(|name| std::env::var(name).ok())
            .map_err(|error| error.to_string())
    }) {
        Ok(config) => Ok(config),
        Err(error) => Err(anyhow!(error.clone())),
    }
}

/// Resolve the configured runner and verify its external dependencies before
/// the server starts accepting launches.
pub async fn validate() -> Result<()> {
    let RunnerConfig::Docker(config) = config()? else {
        return Ok(());
    };
    docker_output(&["version", "--format", "{{.Server.Version}}"])
        .await
        .context("DockerRunner cannot reach the Docker daemon")?;
    for volume in [&config.home_volume, &config.uv_volume] {
        docker_output(&["volume", "inspect", volume])
            .await
            .with_context(|| format!("DockerRunner volume {volume:?} is unavailable"))?;
    }
    docker_output(&["network", "inspect", &config.network])
        .await
        .with_context(|| format!("DockerRunner network {:?} is unavailable", config.network))?;
    let discovered = docker_output(&[
        "ps",
        "--filter",
        &format!("label={SESSION_LABEL}"),
        "--format",
        "{{.Names}}",
    ])
    .await?;
    let count = discovered.lines().filter(|line| !line.is_empty()).count();
    tracing::info!(
        runner = "docker",
        discovered = count,
        "session runner ready"
    );
    Ok(())
}

/// Start a supervisor using the configured placement backend.
pub async fn spawn(opts: &tapestry::LaunchOptions<'_>, memory_max_gb: u64) -> Result<()> {
    match config()? {
        RunnerConfig::Local => tapestry::spawn_detached(opts).await,
        RunnerConfig::Docker(config) => config.spawn(opts, memory_max_gb).await,
    }
}

/// Remove a Docker-placed supervisor after its socket is already gone. Local
/// supervisors have no placement resource to remove.
pub async fn remove(name: &str) -> Result<()> {
    let RunnerConfig::Docker(_) = config()? else {
        return Ok(());
    };
    force_remove_container(&container_name(name)).await
}

impl DockerConfig {
    async fn spawn(&self, opts: &tapestry::LaunchOptions<'_>, memory_max_gb: u64) -> Result<()> {
        let container = container_name(opts.name);
        match container_state(&container).await? {
            Some(true) => {
                if wait_for_supervisor(opts.name).await.is_ok() {
                    return Ok(());
                }
                tracing::warn!(
                    session = %opts.name,
                    %container,
                    "removing a running DockerRunner container without a supervisor socket"
                );
                remove_container(&container).await?;
            }
            Some(false) => remove_container(&container).await?,
            None => {}
        }

        let args = self.create_args(opts, memory_max_gb)?;
        let spec = tapestry::encode_launch_spec(opts, &[("WEAVER_API", &self.api_url)])?;
        let output = Command::new("docker")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("creating DockerRunner container {container}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "could not create DockerRunner container {container}: {}",
                stderr.trim()
            );
        }

        let attach = Command::new("docker")
            .args(["start", "--attach", "--interactive", &container])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut attach = match attach {
            Ok(attach) => attach,
            Err(error) => {
                remove_container(&container).await.ok();
                return Err(error)
                    .with_context(|| format!("attaching to DockerRunner container {container}"));
            }
        };
        let mut stdin = attach
            .stdin
            .take()
            .context("DockerRunner stdin is unavailable")?;
        let delivery = async {
            stdin
                .write_all(&spec)
                .await
                .context("sending launch spec to DockerRunner supervisor")?;
            stdin
                .shutdown()
                .await
                .context("closing DockerRunner supervisor launch input")
        }
        .await;
        drop(stdin);
        if let Err(error) = delivery {
            attach.start_kill().ok();
            attach.wait().await.ok();
            remove_container(&container).await.ok();
            return Err(error);
        }

        if let Err(error) = wait_for_supervisor(opts.name).await {
            let logs = docker_logs(&container)
                .await
                .unwrap_or_else(|log_error| format!("<logs unavailable: {log_error:#}>"));
            attach.start_kill().ok();
            attach.wait().await.ok();
            remove_container(&container).await.ok();
            return Err(error).context(format!(
                "DockerRunner supervisor {container} did not become ready; container logs:\n{logs}"
            ));
        }
        attach.start_kill().ok();
        attach.wait().await.ok();
        tracing::info!(session = %opts.name, %container, "DockerRunner supervisor ready");
        Ok(())
    }

    fn create_args(
        &self,
        opts: &tapestry::LaunchOptions<'_>,
        memory_max_gb: u64,
    ) -> Result<Vec<String>> {
        if !opts.cwd.starts_with(Path::new(CONTAINER_HOME)) {
            bail!(
                "DockerRunner work directory {} is outside {CONTAINER_HOME}",
                opts.cwd.display()
            );
        }
        let workdir = opts
            .cwd
            .to_str()
            .context("DockerRunner work directory is not UTF-8")?;
        let mut args = vec![
            "create".to_string(),
            "--rm".to_string(),
            "--interactive".to_string(),
            "--attach=stdin".to_string(),
            format!("--name={}", container_name(opts.name)),
            format!("--label={SESSION_LABEL}"),
            format!("--label=dev.loom.session={}", opts.name),
            format!("--volume={}:{CONTAINER_HOME}", self.home_volume),
            format!("--volume={}:/opt/uv", self.uv_volume),
            format!("--network={}", self.network),
            "--volume=/var/run/docker.sock:/var/run/docker.sock".to_string(),
            format!("--group-add={}", self.docker_gid),
            "--cap-add=SYS_ADMIN".to_string(),
            "--security-opt=apparmor=unconfined".to_string(),
            "--security-opt=seccomp=unconfined".to_string(),
            "--cgroupns=private".to_string(),
            format!("--workdir={workdir}"),
            format!("--env=WEAVER_HOME={CONTAINER_WEAVER_HOME}"),
            format!("--env=WEAVER_TAPESTRY_DIR={CONTAINER_SOCKET_DIR}"),
            "--env=RUST_BACKTRACE=1".to_string(),
        ];
        if memory_max_gb > 0 {
            args.push(format!("--memory={memory_max_gb}g"));
            args.push(format!("--memory-swap={memory_max_gb}g"));
        }
        args.extend([
            self.image.clone(),
            "tapestry".to_string(),
            "supervise".to_string(),
            "-".to_string(),
        ]);
        Ok(args)
    }
}

async fn wait_for_supervisor(name: &str) -> Result<()> {
    for _ in 0..200 {
        if tapestry::Client::is_alive(name).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    bail!("supervisor for {name} did not come up within 5s")
}

async fn container_state(name: &str) -> Result<Option<bool>> {
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", name])
        .stdin(Stdio::null())
        .output()
        .await
        .context("inspecting DockerRunner container")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if missing_container_error(&stderr) {
            return Ok(None);
        }
        bail!(
            "could not inspect DockerRunner container {name}: {}",
            stderr.trim()
        );
    }
    let value = String::from_utf8(output.stdout).context("Docker returned non-UTF-8 state")?;
    Ok(Some(value.trim() == "true"))
}

fn missing_container_error(stderr: &str) -> bool {
    let normalized = stderr.to_ascii_lowercase();
    normalized.contains("no such object") || normalized.contains("no such container")
}

async fn remove_container(name: &str) -> Result<()> {
    force_remove_container(name).await
}

async fn force_remove_container(name: &str) -> Result<()> {
    let output = Command::new("docker")
        .args(["rm", "--force", name])
        .stdin(Stdio::null())
        .output()
        .await
        .context("removing DockerRunner container")?;
    if output.status.success() {
        return Ok(());
    }
    for _ in 0..40 {
        if container_state(name).await?.is_none() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "could not remove DockerRunner container {name}: {}",
        stderr.trim()
    )
}

async fn docker_output(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("running docker {}", args.first().unwrap_or(&"")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("docker command failed: {}", stderr.trim()));
    }
    String::from_utf8(output.stdout).context("Docker returned non-UTF-8 output")
}

async fn docker_logs(container: &str) -> Result<String> {
    let output = Command::new("docker")
        .args(["logs", container])
        .stdin(Stdio::null())
        .output()
        .await
        .context("reading DockerRunner container logs")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("docker logs failed: {}", stderr.trim());
    }
    let mut logs = String::from_utf8(output.stdout).context("Docker returned non-UTF-8 logs")?;
    logs.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(logs)
}

fn container_name(session: &str) -> String {
    let mut safe: String = session
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    safe.truncate(CONTAINER_SESSION_SLUG_LENGTH);
    let digest = hex::encode(Sha256::digest(session.as_bytes()));
    format!(
        "{SESSION_CONTAINER_PREFIX}{safe}-{}",
        &digest[..CONTAINER_SESSION_HASH_LENGTH]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docker_config() -> DockerConfig {
        DockerConfig {
            image: "registry.example/loom@sha256:abc".to_string(),
            home_volume: "loom_home".to_string(),
            uv_volume: "loom_uv".to_string(),
            docker_gid: "999".to_string(),
            network: "loom_default".to_string(),
            api_url: "http://loom:7878".to_string(),
        }
    }

    #[test]
    fn runner_config_defaults_local_and_rejects_incomplete_docker() {
        assert_eq!(
            RunnerConfig::from_values(|_| None).unwrap(),
            RunnerConfig::Local
        );
        let error =
            RunnerConfig::from_values(|name| (name == "LOOM_RUNNER").then(|| "docker".into()))
                .unwrap_err();
        assert!(error.to_string().contains("LOOM_SESSION_IMAGE"));
    }

    #[test]
    fn docker_create_args_contain_placement_not_launch_secrets() {
        let env = [("API_TOKEN", "super-secret")];
        let opts = tapestry::LaunchOptions {
            name: "weaver/abc",
            cwd: Path::new("/home/app/.weaver/repos/example/.worktrees/abc"),
            script: "agent --secret super-secret",
            env: &env,
            env_clear: true,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Relay,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        let args = docker_config().create_args(&opts, 8).unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains(&format!("--name={}", container_name("weaver/abc"))));
        assert!(rendered.contains("--label=dev.loom.session=weaver/abc"));
        assert!(rendered.contains("--interactive"));
        assert!(rendered.contains("--attach=stdin"));
        assert!(rendered.contains("--network=loom_default"));
        assert!(rendered.contains("--memory=8g"));
        assert!(rendered.contains("registry.example/loom@sha256:abc tapestry supervise -"));
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("API_TOKEN"));
        assert!(!rendered.contains("agent --secret"));
    }

    #[test]
    fn docker_runner_rejects_a_workdir_outside_the_shared_home() {
        let opts = tapestry::LaunchOptions {
            name: "weaver-abc",
            cwd: Path::new("/tmp/repo"),
            script: "true",
            env: &[],
            env_clear: false,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        assert!(docker_config().create_args(&opts, 0).is_err());
    }

    #[test]
    fn docker_container_names_are_bounded_and_do_not_alias_sanitized_sessions() {
        assert_ne!(container_name("weaver/abc"), container_name("weaver-abc"));
        assert!(container_name(&"a".repeat(500)).len() < 80);
    }

    #[test]
    fn docker_inspect_only_treats_missing_containers_as_absent() {
        assert!(missing_container_error(
            "Error: No such object: loom-session-missing"
        ));
        assert!(missing_container_error(
            "Error response from daemon: No such container: loom-session-missing"
        ));
        assert!(!missing_container_error(
            "permission denied while trying to connect to the Docker daemon socket"
        ));
    }
}
