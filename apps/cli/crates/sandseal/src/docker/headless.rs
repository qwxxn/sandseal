//! Running a sandbox with no terminal on either end — scripts, CI, `claude -p` driven by
//! another program. The container gets no TTY and no stdin, and the process's stdout carries
//! the agent's output and nothing else.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::process::Stdio;
use tracing::debug;

/// Keep the real stdout for the agent and send everything else the CLI prints there to
/// stderr instead.
///
/// The start path prints from many places — progress lines, tracing, docker build and
/// compose, user hooks inheriting our stdout — and a consumer parsing the agent's output
/// line by line must not have to tell those apart. Swapping the descriptor once covers all
/// of them, including any added later. The returned copy is close-on-exec, so hooks and
/// docker helpers never hold the consumer's pipe open.
pub fn reserve_stdout() -> Result<OwnedFd> {
    std::io::stdout().flush().ok();

    let agent_stdout = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .context("failed to duplicate stdout")?;
    nix::unistd::dup2(std::io::stderr().as_raw_fd(), std::io::stdout().as_raw_fd())
        .context("failed to redirect stdout to stderr")?;

    Ok(agent_stdout)
}

/// Start the created container attached, relay its output, and return the agent's exit code.
///
/// The container is created but not started (`compose up --no-start`) so that `docker start
/// --attach` is connected before the agent's first byte — attaching to a running container
/// misses whatever it printed in the meantime, and `docker logs` depends on the daemon's
/// logging driver being readable. Without a TTY docker keeps the two streams apart, so the
/// agent's stdout goes to `agent_stdout` and its stderr to ours.
pub async fn run(container_name: &str, agent_stdout: OwnedFd) -> Result<i32> {
    let status = tokio::process::Command::new("docker")
        .args(["start", "--attach", container_name])
        .stdin(Stdio::null())
        .stdout(Stdio::from(agent_stdout))
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("failed to start the sandbox container")?;
    debug!("docker start exited with {}", status.code().unwrap_or(-1));

    // `docker wait` returns 0 at once for a container that never ran, which would report a
    // sandbox that failed to start as an agent that succeeded.
    if container_state(container_name).await? == "created" {
        bail!("the sandbox container did not start (docker start exited with {status})");
    }

    // The attach can end before the agent does (a dropped daemon connection); `docker wait`
    // blocks until it really stops and reports the code the agent exited with.
    let output = tokio::process::Command::new("docker")
        .args(["wait", container_name])
        .stdin(Stdio::null())
        .output()
        .await
        .context("failed to run docker wait")?;
    if !output.status.success() {
        bail!(
            "could not read the agent's exit code: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_exit_code(&String::from_utf8_lossy(&output.stdout))
}

async fn container_state(container_name: &str) -> Result<String> {
    let output = tokio::process::Command::new("docker")
        .args(["inspect", "--format", "{{.State.Status}}", container_name])
        .stdin(Stdio::null())
        .output()
        .await
        .context("failed to inspect the sandbox container")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_exit_code(output: &str) -> Result<i32> {
    output
        .trim()
        .parse()
        .with_context(|| format!("unexpected docker wait output: {output:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_code_is_read_from_docker_wait() {
        assert_eq!(parse_exit_code("0\n").unwrap(), 0);
        assert_eq!(parse_exit_code("137\n").unwrap(), 137);
    }

    #[test]
    fn garbage_from_docker_wait_is_an_error_not_a_success() {
        assert!(parse_exit_code("").is_err());
        assert!(parse_exit_code("Error: No such container").is_err());
    }
}
