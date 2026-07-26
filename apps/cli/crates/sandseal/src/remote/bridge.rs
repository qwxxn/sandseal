use std::io::IsTerminal;
use std::os::unix::io::AsRawFd;

use anyhow::{Context, Result};
use nix::sys::termios;
use nix::pty::{openpty, Winsize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tracing::debug;

use crate::docker::tty;
use crate::remote::overlay::{self, OverlayMessage};
use crate::remote::relay::RelayClient;

/// A terminal-size update from one of the two viewers of the shared PTY.
enum SizeUpdate {
    Local(u16, u16),
    Browser(u16, u16),
}

/// The container has a single TTY size, but local and browser viewers may
/// differ — resize to the smallest of the two so content fits both (tmux-style).
fn combine_sizes(local: Option<(u16, u16)>, browser: Option<(u16, u16)>) -> Option<(u16, u16)> {
    match (local, browser) {
        (Some((lr, lc)), Some((br, bc))) => Some((lr.min(br), lc.min(bc))),
        (Some(size), None) | (None, Some(size)) => Some(size),
        (None, None) => None,
    }
}

/// Wire up terminal-resize propagation for a bridged session: a coordinator that
/// pushes the combined size to the container, a local SIGWINCH watcher, and a
/// forwarder for browser resizes. Returns the sender the relay feeds browser
/// resizes into, or `None` if the Docker API is unreachable (resize is a no-op).
fn spawn_resize_propagation(container_name: &str) -> Option<mpsc::UnboundedSender<(u16, u16)>> {
    let docker = tty::connect()?;
    let name = container_name.to_string();
    let (size_tx, mut size_rx) = mpsc::unbounded_channel::<SizeUpdate>();

    // Coordinator: resize the container to min(local, browser) on every update.
    tokio::spawn(async move {
        let mut local: Option<(u16, u16)> = None;
        let mut browser: Option<(u16, u16)> = None;
        while let Some(update) = size_rx.recv().await {
            match update {
                SizeUpdate::Local(r, c) => local = Some((r, c)),
                SizeUpdate::Browser(r, c) => browser = Some((r, c)),
            }
            if let Some((rows, cols)) = combine_sizes(local, browser) {
                tty::resize(&docker, &name, rows, cols).await;
            }
        }
    });

    // Seed with the current local size, then follow local SIGWINCH.
    if let Some((rows, cols)) = tty::host_terminal_size() {
        let _ = size_tx.send(SizeUpdate::Local(rows, cols));
    }
    let local_size_tx = size_tx.clone();
    tokio::spawn(async move {
        let mut winch = match signal(SignalKind::window_change()) {
            Ok(s) => s,
            Err(e) => {
                debug!("SIGWINCH handler unavailable, local resize disabled: {e}");
                return;
            }
        };
        while winch.recv().await.is_some() {
            if let Some((rows, cols)) = tty::host_terminal_size() {
                let _ = local_size_tx.send(SizeUpdate::Local(rows, cols));
            }
        }
    });

    // Forward browser resizes (delivered by the relay) into the coordinator.
    let (browser_tx, mut browser_rx) = mpsc::unbounded_channel::<(u16, u16)>();
    tokio::spawn(async move {
        while let Some((rows, cols)) = browser_rx.recv().await {
            let _ = size_tx.send(SizeUpdate::Browser(rows, cols));
        }
    });

    Some(browser_tx)
}

struct RawModeGuard {
    orig: termios::Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(&std::io::stdin(), termios::SetArg::TCSANOW, &self.orig);
    }
}

struct StderrRedirect {
    saved_fd: i32,
}

impl StderrRedirect {
    fn to_devnull() -> Option<Self> {
        let stderr_fd = std::io::stderr().as_raw_fd();
        let saved = nix::unistd::dup(stderr_fd).ok()?;
        let devnull = std::fs::File::open("/dev/null").ok()?;
        nix::unistd::dup2(devnull.as_raw_fd(), stderr_fd).ok()?;
        Some(Self { saved_fd: saved })
    }
}

impl Drop for StderrRedirect {
    fn drop(&mut self) {
        let stderr_fd = std::io::stderr().as_raw_fd();
        let _ = nix::unistd::dup2(self.saved_fd, stderr_fd);
        let _ = nix::unistd::close(self.saved_fd);
    }
}

/// Attach to a running Docker container and bridge its stdin/stdout to both
/// the local terminal (interactive) and the relay (for web dashboard access).
///
/// Uses a host-side PTY so docker attach sees a real terminal.
/// Output from the PTY master is forwarded to both the local terminal
/// and the encrypted relay.
pub async fn bridge_container(
    container_name: &str,
    relay_url: String,
    relay_token: String,
) -> Result<()> {
    let (rows, cols) = tty::host_terminal_size().unwrap_or((24, 80));
    let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };

    let pty = openpty(&ws, None).context("openpty")?;

    let slave_clone1 = pty.slave.try_clone().context("dup slave")?;
    let slave_clone2 = pty.slave.try_clone().context("dup slave")?;

    let mut child = Command::new("docker")
        .args(["attach", "--sig-proxy=false", container_name])
        .stdin(std::process::Stdio::from(pty.slave))
        .stdout(std::process::Stdio::from(slave_clone1))
        .stderr(std::process::Stdio::from(slave_clone2))
        .kill_on_drop(true)
        .spawn()
        .context("failed to attach to container")?;

    let _raw_guard = if std::io::stdin().is_terminal() {
        let stdin = std::io::stdin();
        let orig = termios::tcgetattr(&stdin).context("tcgetattr")?;
        let mut raw = orig.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&stdin, termios::SetArg::TCSANOW, &raw).context("tcsetattr")?;
        Some(RawModeGuard { orig })
    } else {
        debug!("stdin is not a terminal, skipping raw mode");
        None
    };

    // Suppress tracing stderr output during interactive session (would corrupt TUI)
    let _stderr_guard = StderrRedirect::to_devnull();

    // Container stdin receives from both local keyboard and relay
    let (to_container_tx, mut to_container_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    // Container stdout goes to relay (relay handles encryption)
    let (to_relay_tx, to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Overlay for transient status messages
    let (overlay_tx, overlay_rx) = mpsc::unbounded_channel::<OverlayMessage>();
    let overlay_handle = tokio::spawn(overlay::run_overlay(overlay_rx));

    // Terminal resize: local SIGWINCH + browser resizes → container TTY.
    let browser_resize_tx = spawn_resize_propagation(container_name);

    // Background: relay connection + key exchange + encrypted bridge
    let relay_from_tx = to_container_tx.clone();
    let relay_handle = tokio::spawn(async move {
        let relay = RelayClient::new(relay_url, relay_token);
        let _ = relay
            .connect_and_run(to_relay_rx, relay_from_tx, Some(overlay_tx), browser_resize_tx)
            .await;
    });

    // PTY master read → local terminal + relay (blocking thread — PTY fds aren't async-friendly)
    let master_read = pty.master.try_clone().context("dup master")?;
    let read_handle = {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut file = std::fs::File::from(master_read);
            let mut buf = [0u8; 4096];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() { break; }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(&data);
                    let _ = out.flush();
                }
                let _ = to_relay_tx.send(data);
            }
        })
    };

    // PTY master write (receives from local keyboard + relay)
    let master_write = pty.master;
    let write_handle = std::thread::spawn(move || {
        use std::io::Write;
        let mut file = std::fs::File::from(master_write);
        while let Some(data) = to_container_rx.blocking_recv() {
            if file.write_all(&data).is_err() { break; }
        }
    });

    // Local stdin → container (via PTY)
    let local_tx = to_container_tx;
    let stdin_handle = tokio::spawn(async move {
        let mut reader = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if local_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for docker attach to exit (container session ended)
    let _ = child.wait().await;

    relay_handle.abort();
    overlay_handle.abort();
    stdin_handle.abort();
    read_handle.abort();
    drop(write_handle);

    Ok(())
}
