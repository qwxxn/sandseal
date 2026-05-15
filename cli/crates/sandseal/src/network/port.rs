use anyhow::{bail, Result};
use std::net::TcpListener;

const TTYD_PORT_MIN: u16 = 7690;
const TTYD_PORT_MAX: u16 = 7699;

/// Allocate a free port in the ttyd range (7690-7699).
/// Checks both Docker labels and actual port occupancy.
pub fn allocate_ttyd_port(used_ports: &[u16]) -> Result<u16> {
    for port in TTYD_PORT_MIN..=TTYD_PORT_MAX {
        if used_ports.contains(&port) {
            continue;
        }
        if !is_port_in_use(port) {
            return Ok(port);
        }
    }
    bail!("no free ttyd port available in range {TTYD_PORT_MIN}-{TTYD_PORT_MAX}");
}

/// Check if a TCP port is currently in use by attempting to bind it.
pub fn is_port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// Query Docker for ports used by existing sandseal instances.
pub fn get_used_ttyd_ports() -> Result<Vec<u16>> {
    let output = std::process::Command::new("docker")
        .args(["ps", "--filter", "label=sandseal.ttyd_port", "--format", "{{.Labels}}"])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();

    for line in stdout.lines() {
        for label in line.split(',') {
            if let Some(port_str) = label.strip_prefix("sandseal.ttyd_port=") {
                if let Ok(port) = port_str.parse::<u16>() {
                    ports.push(port);
                }
            }
        }
    }

    Ok(ports)
}
