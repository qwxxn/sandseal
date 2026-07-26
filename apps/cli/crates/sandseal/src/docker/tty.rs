//! Terminal-size propagation to the sandbox container.
//!
//! `docker attach` only sets the container TTY size once and ignores later
//! SIGWINCH events (moby/moby#33794), so we push every resize to the daemon
//! ourselves via the `ContainerResize` API. The kernel then delivers SIGWINCH
//! to the agent (PID 1) which re-renders at the new size.

use std::io::IsTerminal;
use std::os::unix::io::AsRawFd;

use bollard::query_parameters::ResizeContainerTTYOptionsBuilder;
use bollard::Docker;
use tracing::debug;

/// Read the controlling terminal size as `(rows, cols)` via TIOCGWINSZ on stdin.
/// Returns `None` when stdin is not a TTY or the ioctl yields a zero size.
pub fn host_terminal_size() -> Option<(u16, u16)> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return Some((ws.ws_row, ws.ws_col));
        }
    }
    None
}

/// Connect to the local Docker daemon, honouring `DOCKER_HOST` / contexts.
/// Returns `None` (with a debug log) on failure — resize is best-effort and
/// must never prevent a session from starting.
pub fn connect() -> Option<Docker> {
    match Docker::connect_with_local_defaults() {
        Ok(docker) => Some(docker),
        Err(e) => {
            debug!("docker API connect failed, terminal resize disabled: {e}");
            None
        }
    }
}

/// Push a new TTY size to the container. Errors are swallowed: a failed resize
/// must not tear down the session.
pub async fn resize(docker: &Docker, container: &str, rows: u16, cols: u16) {
    let opts = ResizeContainerTTYOptionsBuilder::new()
        .h(rows as i32)
        .w(cols as i32)
        .build();
    if let Err(e) = docker.resize_container_tty(container, opts).await {
        debug!("container resize to {rows}x{cols} failed: {e}");
    }
}
