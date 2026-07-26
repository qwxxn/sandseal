use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, error, info};

use crate::docker::tty;

/// Environment variables needed by the base docker-compose.yml template.
pub struct ComposeEnv {
    pub project_dir: String,
}

/// Build the base docker compose command with project name and compose files.
pub fn compose_cmd(
    instance_name: &str,
    script_dir: &Path,
    override_file: &Path,
) -> Vec<String> {
    vec![
        "docker".into(),
        "compose".into(),
        "-p".into(),
        instance_name.into(),
        "--project-directory".into(),
        script_dir.to_string_lossy().into(),
        "-f".into(),
        script_dir.join("agents/docker-compose.yml").to_string_lossy().into(),
        "-f".into(),
        override_file.to_string_lossy().into(),
    ]
}

/// Run `docker compose up -d agent` (images are prebuilt by the image module).
pub fn compose_up(cmd: &[String], env: &ComposeEnv) -> Result<()> {
    let mut args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    args.push("up");
    args.push("-d");
    args.push("agent");

    debug!("running: {}", args.join(" "));

    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .env("PROJECT_DIR", &env.project_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = command.status().context("failed to run docker compose up")?;

    if !status.success() {
        bail!("docker compose up failed with exit code: {}", status.code().unwrap_or(-1));
    }

    Ok(())
}

/// Run `docker compose down --rmi local`
pub fn compose_down(cmd: &[String]) -> Result<()> {
    let mut args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    args.extend(["down", "--rmi", "local"]);

    debug!("running: {}", args.join(" "));

    let status = Command::new(&args[0])
        .args(&args[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run docker compose down")?;

    if !status.success() {
        error!("docker compose down failed (exit {})", status.code().unwrap_or(-1));
    }

    Ok(())
}

/// Wait for the container to be in "running" state, then attach.
pub async fn wait_and_attach(container_name: &str) -> Result<()> {
    let max_retries = 20;
    let delay = Duration::from_millis(500);

    for attempt in 1..=max_retries {
        let output = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Status}}", container_name])
            .output()
            .context("failed to inspect container")?;

        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("container state (attempt {attempt}/{max_retries}): {state}");

        match state.as_str() {
            "running" => {
                info!("container is running, attaching...");
                return attach(container_name).await;
            }
            "exited" | "dead" => {
                let logs = Command::new("docker")
                    .args(["logs", "--tail", "50", container_name])
                    .output()?;
                let stderr = String::from_utf8_lossy(&logs.stderr);
                let stdout = String::from_utf8_lossy(&logs.stdout);
                bail!(
                    "container exited before attach.\nstdout:\n{stdout}\nstderr:\n{stderr}"
                );
            }
            _ => {
                tokio::time::sleep(delay).await;
            }
        }
    }

    bail!("container did not reach running state after {max_retries} retries");
}

/// Attach to a running container (interactive), propagating terminal resizes.
///
/// `docker attach` only sets the TTY size on connect, so we push the initial
/// size and every subsequent SIGWINCH to the container via the Docker API.
async fn attach(container_name: &str) -> Result<()> {
    let docker = tty::connect();

    // Set the correct size before the agent draws its first frame.
    if let (Some(docker), Some((rows, cols))) = (&docker, tty::host_terminal_size()) {
        tty::resize(docker, container_name, rows, cols).await;
    }

    // Forward later resizes for as long as the session is attached.
    let resize_task = docker.map(|docker| {
        let name = container_name.to_string();
        tokio::spawn(async move {
            let mut winch = match signal(SignalKind::window_change()) {
                Ok(s) => s,
                Err(e) => {
                    debug!("SIGWINCH handler unavailable, resize disabled: {e}");
                    return;
                }
            };
            while winch.recv().await.is_some() {
                if let Some((rows, cols)) = tty::host_terminal_size() {
                    tty::resize(&docker, &name, rows, cols).await;
                }
            }
        })
    });

    let mut child = tokio::process::Command::new("docker")
        .args(["attach", "--sig-proxy=false", container_name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to attach to container")?;

    let status = child.wait().await.context("docker attach failed")?;

    if let Some(task) = resize_task {
        task.abort();
    }

    debug!("container exited with code: {}", status.code().unwrap_or(-1));
    Ok(())
}

/// Get the container name for a compose project.
pub fn get_container_name(instance_name: &str) -> Result<String> {
    let output = Command::new("docker")
        .args([
            "ps",
            "--filter", &format!("label=com.docker.compose.project={instance_name}"),
            "--format", "{{.Names}}",
        ])
        .output()
        .context("failed to query container name")?;

    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        bail!("no container found for project {instance_name}");
    }

    Ok(name)
}
