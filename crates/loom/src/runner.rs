//! Placement of long-lived Tapestry supervisors.
//!
//! Transport remains the shared Unix socket and relay spool under
//! `WEAVER_TAPESTRY_DIR`. A runner only decides where the supervisor process
//! lives. Production uses a Docker container per supervisor so replacing the
//! Loom control-plane container does not kill live agents; local development
//! keeps the detached-process behavior.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bollard::errors::Error as DockerError;
use bollard::models::{ContainerCreateBody, HostConfig, HostConfigCgroupnsModeEnum};
use bollard::query_parameters::{
    AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, ListContainersOptionsBuilder,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder,
};
use bollard::Docker;
use futures_util::TryStreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

const SESSION_LABEL_KEY: &str = "dev.loom.runner";
const SESSION_LABEL_VALUE: &str = "session";
const SESSION_NAME_LABEL: &str = "dev.loom.session";
const SESSION_CONTAINER_PREFIX: &str = "loom-session-";
const CONTAINER_HOME: &str = "/home/app";
const CONTAINER_WEAVER_HOME: &str = "/home/app/.weaver";
const CONTAINER_SOCKET_DIR: &str = "/home/app/.weaver/sock";
const CONTAINER_SESSION_SLUG_LENGTH: usize = 48;
const CONTAINER_SESSION_HASH_LENGTH: usize = 12;
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;

static RUNNER: OnceCell<Arc<dyn Runner>> = OnceCell::const_new();

/// Places and removes Tapestry supervisors without changing their socket
/// transport protocol.
#[async_trait]
pub trait Runner: Send + Sync {
    /// Verify that the placement backend is ready to accept launches.
    async fn validate(&self) -> Result<()>;

    /// Start or adopt the supervisor described by `opts`.
    async fn start(&self, opts: &tapestry::LaunchOptions<'_>, memory_max_gb: u64) -> Result<()>;

    /// Remove placement resources for a supervisor whose socket is gone.
    async fn remove(&self, name: &str) -> Result<()>;
}

/// Runs supervisors as detached processes on the Loom host.
pub struct ProcessRunner;

#[async_trait]
impl Runner for ProcessRunner {
    async fn validate(&self) -> Result<()> {
        Ok(())
    }

    async fn start(&self, opts: &tapestry::LaunchOptions<'_>, _memory_max_gb: u64) -> Result<()> {
        tapestry::spawn_detached(opts).await
    }

    async fn remove(&self, _name: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerConfig {
    image: String,
    home_volume: String,
    uv_volume: String,
    docker_gid: String,
    network: String,
    api_url: String,
}

/// Runs each supervisor in its own Docker container.
pub struct ContainerRunner {
    docker: Docker,
    config: ContainerConfig,
}

impl ContainerRunner {
    async fn connect(config: ContainerConfig) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .context("connecting to the Docker socket")?
            .negotiate_version()
            .await
            .context("negotiating the Docker Engine API version")?;
        Ok(Self { docker, config })
    }

    async fn container_state(&self, session: &str) -> Result<Option<bool>> {
        let container = container_name(session);
        let inspected = match self.docker.inspect_container(&container, None).await {
            Ok(inspected) => inspected,
            Err(error) if docker_not_found(&error) => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting session container {container}"))
            }
        };
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        if labels.get(SESSION_LABEL_KEY).map(String::as_str) != Some(SESSION_LABEL_VALUE)
            || labels.get(SESSION_NAME_LABEL).map(String::as_str) != Some(session)
        {
            bail!("container {container} exists but is not the Loom supervisor for {session}");
        }
        Ok(Some(
            inspected
                .state
                .and_then(|state| state.running)
                .unwrap_or(false),
        ))
    }

    async fn force_remove(&self, container: &str) -> Result<()> {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        match self.docker.remove_container(container, Some(options)).await {
            Ok(()) => Ok(()),
            Err(error) if docker_not_found(&error) => Ok(()),
            Err(error) if docker_conflict(&error) => {
                for _ in 0..40 {
                    match self.docker.inspect_container(container, None).await {
                        Err(inspect_error) if docker_not_found(&inspect_error) => return Ok(()),
                        Err(inspect_error) => {
                            return Err(inspect_error).with_context(|| {
                                format!("checking removal of session container {container}")
                            })
                        }
                        Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
                    }
                }
                Err(error).with_context(|| format!("removing session container {container}"))
            }
            Err(error) => {
                Err(error).with_context(|| format!("removing session container {container}"))
            }
        }
    }

    async fn cleanup_after_failure(&self, container: &str) {
        if let Err(error) = self.force_remove(container).await {
            tracing::warn!(%container, %error, "could not clean up failed session container");
        }
    }

    async fn logs(&self, container: &str) -> Result<String> {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .build();
        let output = self
            .docker
            .logs(container, Some(options))
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| format!("reading logs for session container {container}"))?;
        Ok(output.into_iter().map(|line| line.to_string()).collect())
    }

    fn create_body(
        &self,
        opts: &tapestry::LaunchOptions<'_>,
        memory_max_gb: u64,
    ) -> Result<ContainerCreateBody> {
        if !opts.cwd.starts_with(Path::new(CONTAINER_HOME)) {
            bail!(
                "ContainerRunner work directory {} is outside {CONTAINER_HOME}",
                opts.cwd.display()
            );
        }
        let workdir = opts
            .cwd
            .to_str()
            .context("ContainerRunner work directory is not UTF-8")?;
        let memory = if memory_max_gb == 0 {
            None
        } else {
            let bytes = memory_max_gb
                .checked_mul(BYTES_PER_GIB)
                .context("ContainerRunner memory limit is too large")?;
            Some(i64::try_from(bytes).context("ContainerRunner memory limit is too large")?)
        };
        let labels = HashMap::from([
            (
                SESSION_LABEL_KEY.to_string(),
                SESSION_LABEL_VALUE.to_string(),
            ),
            (SESSION_NAME_LABEL.to_string(), opts.name.to_string()),
        ]);
        let host_config = HostConfig {
            auto_remove: Some(true),
            binds: Some(vec![
                format!("{}:{CONTAINER_HOME}", self.config.home_volume),
                format!("{}:/opt/uv", self.config.uv_volume),
                "/var/run/docker.sock:/var/run/docker.sock".to_string(),
            ]),
            cap_add: Some(vec!["SYS_ADMIN".to_string()]),
            cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
            group_add: Some(vec![self.config.docker_gid.clone()]),
            memory,
            memory_swap: memory,
            network_mode: Some(self.config.network.clone()),
            security_opt: Some(vec![
                "apparmor=unconfined".to_string(),
                "seccomp=unconfined".to_string(),
            ]),
            ..Default::default()
        };
        Ok(ContainerCreateBody {
            attach_stdin: Some(true),
            open_stdin: Some(true),
            stdin_once: Some(false),
            env: Some(vec![
                format!("WEAVER_HOME={CONTAINER_WEAVER_HOME}"),
                format!("WEAVER_TAPESTRY_DIR={CONTAINER_SOCKET_DIR}"),
                "RUST_BACKTRACE=1".to_string(),
            ]),
            cmd: Some(vec![
                "tapestry".to_string(),
                "supervise".to_string(),
                "-".to_string(),
            ]),
            image: Some(self.config.image.clone()),
            working_dir: Some(workdir.to_string()),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        })
    }
}

#[async_trait]
impl Runner for ContainerRunner {
    async fn validate(&self) -> Result<()> {
        for volume in [&self.config.home_volume, &self.config.uv_volume] {
            self.docker
                .inspect_volume(volume)
                .await
                .with_context(|| format!("ContainerRunner volume {volume:?} is unavailable"))?;
        }
        self.docker
            .inspect_network(&self.config.network, None)
            .await
            .with_context(|| {
                format!(
                    "ContainerRunner network {:?} is unavailable",
                    self.config.network
                )
            })?;
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{SESSION_LABEL_KEY}={SESSION_LABEL_VALUE}")],
        )]);
        let options = ListContainersOptionsBuilder::default()
            .filters(&filters)
            .build();
        let discovered = self.docker.list_containers(Some(options)).await?;
        tracing::info!(
            runner = "container",
            discovered = discovered.len(),
            "session runner ready"
        );
        Ok(())
    }

    async fn start(&self, opts: &tapestry::LaunchOptions<'_>, memory_max_gb: u64) -> Result<()> {
        let container = container_name(opts.name);
        match self.container_state(opts.name).await? {
            Some(true) => {
                if wait_for_supervisor(opts.name).await.is_ok() {
                    return Ok(());
                }
                tracing::warn!(
                    session = %opts.name,
                    %container,
                    "removing a running ContainerRunner container without a supervisor socket"
                );
                self.force_remove(&container).await?;
            }
            Some(false) => self.force_remove(&container).await?,
            None => {}
        }

        let body = self.create_body(opts, memory_max_gb)?;
        let spec = tapestry::encode_launch_spec(opts, &[("WEAVER_API", &self.config.api_url)])?;
        let options = CreateContainerOptionsBuilder::default()
            .name(&container)
            .build();
        let created = self
            .docker
            .create_container(Some(options), body)
            .await
            .with_context(|| format!("creating session container {container}"))?;
        for warning in created.warnings {
            tracing::warn!(%container, %warning, "Docker reported a container creation warning");
        }

        let attach_options = AttachContainerOptionsBuilder::default()
            .stdin(true)
            .stdout(true)
            .stderr(true)
            .stream(true)
            .build();
        let attached = match self
            .docker
            .attach_container(&created.id, Some(attach_options))
            .await
        {
            Ok(attached) => attached,
            Err(error) => {
                self.cleanup_after_failure(&created.id).await;
                return Err(error).context("attaching launch input to session container");
            }
        };
        let bollard::container::AttachContainerResults {
            mut output,
            mut input,
        } = attached;
        let output_task = tokio::spawn(async move {
            while output.try_next().await?.is_some() {}
            Ok::<(), DockerError>(())
        });
        if let Err(error) = self.docker.start_container(&created.id, None).await {
            drop(input);
            output_task.abort();
            self.cleanup_after_failure(&created.id).await;
            return Err(error).context("starting session container");
        }
        let delivery = async {
            input
                .write_all(&spec)
                .await
                .context("sending launch spec to session supervisor")?;
            input
                .flush()
                .await
                .context("flushing session supervisor launch input")
        }
        .await;
        if let Err(error) = delivery {
            drop(input);
            output_task.abort();
            self.cleanup_after_failure(&created.id).await;
            return Err(error);
        }

        if let Err(error) = wait_for_supervisor(opts.name).await {
            drop(input);
            output_task.abort();
            let logs = self
                .logs(&created.id)
                .await
                .unwrap_or_else(|log_error| format!("<logs unavailable: {log_error:#}>"));
            self.cleanup_after_failure(&created.id).await;
            return Err(error).context(format!(
                "ContainerRunner supervisor {container} did not become ready; container logs:\n{logs}"
            ));
        }
        drop(input);
        output_task.abort();
        tracing::info!(session = %opts.name, %container, "ContainerRunner supervisor ready");
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<()> {
        if self.container_state(name).await?.is_some() {
            self.force_remove(&container_name(name)).await?;
        }
        Ok(())
    }
}

fn container_config(mut value: impl FnMut(&str) -> Option<String>) -> Result<ContainerConfig> {
    Ok(ContainerConfig {
        image: required_value(&mut value, "LOOM_SESSION_IMAGE")?,
        home_volume: required_value(&mut value, "LOOM_SESSION_HOME_VOLUME")?,
        uv_volume: required_value(&mut value, "LOOM_SESSION_UV_VOLUME")?,
        docker_gid: docker_gid(&mut value)?,
        network: required_value(&mut value, "LOOM_SESSION_NETWORK")?,
        api_url: required_value(&mut value, "LOOM_SESSION_API_URL")?,
    })
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

async fn runner_from_env() -> Result<Arc<dyn Runner>> {
    match runner_name(std::env::var("LOOM_RUNNER").ok())?.as_str() {
        "local" => Ok(Arc::new(ProcessRunner)),
        "docker" => {
            let config = container_config(|name| std::env::var(name).ok())?;
            Ok(Arc::new(ContainerRunner::connect(config).await?))
        }
        _ => unreachable!("runner_name validates the runner name"),
    }
}

fn runner_name(value: Option<String>) -> Result<String> {
    let name = value.unwrap_or_else(|| "local".to_string());
    if !matches!(name.as_str(), "local" | "docker") {
        bail!("unknown LOOM_RUNNER {name:?}; expected local or docker");
    }
    Ok(name)
}

async fn configured_runner() -> Result<&'static dyn Runner> {
    let runner = RUNNER.get_or_try_init(runner_from_env).await?;
    Ok(runner.as_ref())
}

/// Resolve the configured runner and verify its external dependencies before
/// the server starts accepting launches.
pub async fn validate() -> Result<()> {
    configured_runner().await?.validate().await
}

/// Start a supervisor using the configured placement backend.
pub async fn spawn(opts: &tapestry::LaunchOptions<'_>, memory_max_gb: u64) -> Result<()> {
    configured_runner().await?.start(opts, memory_max_gb).await
}

/// Remove placement resources after a supervisor's socket is gone.
pub async fn remove(name: &str) -> Result<()> {
    configured_runner().await?.remove(name).await
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

fn docker_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn docker_conflict(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 409,
            ..
        }
    )
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

    fn container_config_fixture() -> ContainerConfig {
        ContainerConfig {
            image: "registry.example/loom@sha256:abc".to_string(),
            home_volume: "loom_home".to_string(),
            uv_volume: "loom_uv".to_string(),
            docker_gid: "999".to_string(),
            network: "loom_default".to_string(),
            api_url: "http://loom:7878".to_string(),
        }
    }

    fn container_runner() -> ContainerRunner {
        ContainerRunner {
            docker: Docker::connect_with_socket_defaults().unwrap(),
            config: container_config_fixture(),
        }
    }

    fn launch_options<'a>(
        cwd: &'a Path,
        env: &'a [(&'a str, &'a str)],
    ) -> tapestry::LaunchOptions<'a> {
        tapestry::LaunchOptions {
            name: "weaver/abc",
            cwd,
            script: "agent --secret super-secret",
            env,
            env_clear: true,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Relay,
            segment_max_bytes: None,
            supervisor_bin: None,
        }
    }

    #[test]
    fn container_config_rejects_missing_values_and_nonnumeric_gid() {
        let error = container_config(|_| None).unwrap_err();
        assert!(error.to_string().contains("LOOM_SESSION_IMAGE"));

        let error = container_config(|name| {
            let value = match name {
                "LOOM_SESSION_IMAGE" => "loom:latest",
                "LOOM_SESSION_HOME_VOLUME" => "loom_home",
                "LOOM_SESSION_UV_VOLUME" => "loom_uv",
                "LOOM_SESSION_DOCKER_GID" => "docker",
                "LOOM_SESSION_NETWORK" => "loom_default",
                "LOOM_SESSION_API_URL" => "http://loom:7878",
                _ => return None,
            };
            Some(value.to_string())
        })
        .unwrap_err();
        assert!(error.to_string().contains("must be numeric"));
    }

    #[test]
    fn runner_defaults_to_local_and_rejects_unknown_names() {
        assert_eq!(runner_name(None).unwrap(), "local");
        assert_eq!(runner_name(Some("docker".into())).unwrap(), "docker");
        assert!(runner_name(Some("remote".into())).is_err());
    }

    #[test]
    fn container_create_body_contains_placement_not_launch_secrets() {
        let env = [("API_TOKEN", "super-secret")];
        let cwd = Path::new("/home/app/.weaver/repos/example/.worktrees/abc");
        let body = container_runner()
            .create_body(&launch_options(cwd, &env), 8)
            .unwrap();

        assert_eq!(
            body.labels.as_ref().unwrap().get(SESSION_NAME_LABEL),
            Some(&"weaver/abc".to_string())
        );
        assert_eq!(
            body.cmd,
            Some(vec!["tapestry".into(), "supervise".into(), "-".into()])
        );
        assert_eq!(body.attach_stdin, Some(true));
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(body.stdin_once, Some(false));
        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.network_mode.as_deref(), Some("loom_default"));
        assert_eq!(host.memory, Some(8 * BYTES_PER_GIB as i64));
        assert_eq!(host.memory_swap, host.memory);
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(host.group_add.as_deref(), Some(&["999".to_string()][..]));
        assert_eq!(
            host.cgroupns_mode,
            Some(HostConfigCgroupnsModeEnum::PRIVATE)
        );
        assert!(host
            .binds
            .as_ref()
            .unwrap()
            .contains(&"/var/run/docker.sock:/var/run/docker.sock".to_string()));

        let rendered = serde_json::to_string(&body).unwrap();
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("API_TOKEN"));
        assert!(!rendered.contains("agent --secret"));
    }

    #[test]
    fn container_runner_rejects_a_workdir_outside_the_shared_home() {
        let opts = launch_options(Path::new("/tmp/repo"), &[]);
        assert!(container_runner().create_body(&opts, 0).is_err());
    }

    #[test]
    fn container_names_are_bounded_and_do_not_alias_sanitized_sessions() {
        assert_ne!(container_name("weaver/abc"), container_name("weaver-abc"));
        assert!(container_name(&"a".repeat(500)).len() < 80);
    }

    #[test]
    fn only_docker_404_is_absence() {
        assert!(docker_not_found(&DockerError::DockerResponseServerError {
            status_code: 404,
            message: "missing".to_string(),
        }));
        assert!(!docker_not_found(&DockerError::DockerResponseServerError {
            status_code: 500,
            message: "daemon failed".to_string(),
        }));
        assert!(!docker_not_found(&DockerError::RequestTimeoutError));
        assert!(docker_conflict(&DockerError::DockerResponseServerError {
            status_code: 409,
            message: "removal already in progress".to_string(),
        }));
        assert!(!docker_conflict(&DockerError::DockerResponseServerError {
            status_code: 500,
            message: "daemon failed".to_string(),
        }));
    }
}
