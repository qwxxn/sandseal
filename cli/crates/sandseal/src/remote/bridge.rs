use std::io::IsTerminal;
use std::os::unix::io::AsRawFd;

use anyhow::{Context, Result};
use nix::sys::termios;
use nix::pty::{openpty, Winsize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::debug;

use crate::remote::overlay::{self, OverlayMessage};
use crate::remote::relay::RelayClient;

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

fn get_terminal_size() -> Winsize {
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 {
            return Winsize {
                ws_row: ws.ws_row,
                ws_col: ws.ws_col,
                ws_xpixel: ws.ws_xpixel,
                ws_ypixel: ws.ws_ypixel,
            };
        }
    }
    Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 }
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
    let ws = if std::io::stdin().is_terminal() {
        get_terminal_size()
    } else {
        Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 }
    };

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

    // Background: relay connection + key exchange + encrypted bridge
    let relay_from_tx = to_container_tx.clone();
    let relay_handle = tokio::spawn(async move {
        let relay = RelayClient::new(relay_url, relay_token);
        let _ = relay.connect_and_run(to_relay_rx, relay_from_tx, Some(overlay_tx)).await;
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
