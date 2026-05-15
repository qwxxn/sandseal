use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info};

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

/// Run `docker compose up -d [--build] agent`
pub fn compose_up(cmd: &[String], rebuild: bool) -> Result<()> {
    let mut args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    args.push("up");
    args.push("-d");
    if rebuild {
        args.push("--build");
    }
    args.push("agent");

    debug!("running: {}", args.join(" "));

    let status = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to run docker compose up")?;

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
pub fn wait_and_attach(container_name: &str) -> Result<()> {
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
                return attach(container_name);
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
                thread::sleep(delay);
            }
        }
    }

    bail!("container did not reach running state after {max_retries} retries");
}

/// Attach to a running container (interactive).
fn attach(container_name: &str) -> Result<()> {
    let status = Command::new("docker")
        .args(["attach", container_name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to attach to container")?;

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
