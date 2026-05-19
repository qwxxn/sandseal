use std::io::IsTerminal;

use anyhow::{Context, Result};
use nix::sys::termios;
use nix::pty::{openpty, Winsize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::remote::relay::RelayClient;

struct RawModeGuard {
    orig: termios::Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(&std::io::stdin(), termios::SetArg::TCSANOW, &self.orig);
    }
}

fn get_terminal_size() -> Winsize {
    use std::os::unix::io::AsRawFd;
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
/// Uses a host-side PTY so docker attach sees a real terminal (required for
/// the container's tmux session). Output from the PTY master is forwarded to
/// both the local terminal and the encrypted relay.
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
        .args(["attach", container_name])
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

    // Container stdin receives from both local keyboard and relay
    let (to_container_tx, mut to_container_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    // Container stdout goes to relay (relay handles encryption)
    let (to_relay_tx, to_relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Background: relay connection + key exchange + encrypted bridge
    let relay_from_tx = to_container_tx.clone();
    let relay_handle = tokio::spawn(async move {
        let relay = RelayClient::new(relay_url, relay_token);
        if let Err(e) = relay.connect_and_run(to_relay_rx, relay_from_tx).await {
            error!("relay: {e}");
        }
        debug!("relay task ended");
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
                    Err(e) => {
                        debug!("pty master read error: {e}");
                        break;
                    }
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
            debug!("pty master read ended");
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
        debug!("pty master write ended");
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
        debug!("local stdin ended");
    });

    // Wait for docker attach to exit (container session ended)
    let status = child.wait().await;
    match &status {
        Ok(s) => info!("session ended (exit {})", s.code().unwrap_or(-1)),
        Err(e) => warn!("wait error: {e}"),
    }

    relay_handle.abort();
    stdin_handle.abort();
    read_handle.abort();
    drop(write_handle);

    Ok(())
}
