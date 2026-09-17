//! Reading the clipboard on the host, with whatever the platform offers.
//!
//! Each platform gets the same three questions — does it hold an image, does it hold text,
//! give me one of them — answered with the tools Claude Code itself uses natively, so a paste
//! that works outside the sandbox works inside it.
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use tracing::debug;

use super::Target;
use super::server::Source;

/// Long enough for PowerShell to spin up; short enough that a hung tool does not stall a paste.
const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

const WINDOWS_POWERSHELL: &str = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux under WSL2: the clipboard is Windows', reached through `powershell.exe`.
    Wsl,
    /// X11 or Wayland: `xclip` and `wl-paste`.
    Linux,
    /// `osascript` and `pbpaste`.
    MacOs,
}

pub struct HostClipboard {
    platform: Platform,
}

impl HostClipboard {
    /// None when the platform has no clipboard the CLI knows how to read.
    pub fn detect() -> Option<Self> {
        detect_platform().map(|platform| Self { platform })
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }
}

fn detect_platform() -> Option<Platform> {
    if cfg!(target_os = "macos") {
        return Some(Platform::MacOs);
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let version = std::fs::read_to_string("/proc/version").unwrap_or_default().to_lowercase();
    if version.contains("microsoft") || version.contains("wsl") {
        Some(Platform::Wsl)
    } else {
        Some(Platform::Linux)
    }
}

impl Source for HostClipboard {
    async fn targets(&self) -> Vec<Target> {
        match self.platform {
            Platform::Wsl => wsl::targets().await,
            Platform::Linux => linux::targets().await,
            Platform::MacOs => macos::targets().await,
        }
    }

    async fn read(&self, target: Target) -> Result<Option<Vec<u8>>> {
        match self.platform {
            Platform::Wsl => wsl::read(target).await,
            Platform::Linux => linux::read(target).await,
            Platform::MacOs => macos::read(target).await,
        }
    }
}

/// Runs a tool and returns its stdout, or None when it exited non-zero — which for every
/// clipboard tool means "nothing of that kind on the clipboard", not a failure.
async fn run(program: &str, args: &[&str]) -> Result<Option<Vec<u8>>> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let output = tokio::time::timeout(TOOL_TIMEOUT, child)
        .await
        .with_context(|| format!("{program} did not answer within {}s", TOOL_TIMEOUT.as_secs()))?
        .with_context(|| format!("failed to run {program}"))?;

    if !output.status.success() {
        debug!("{program} exited with {}", output.status);
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

/// Same, for a tool that may simply not be installed: a missing binary reads as "nothing".
async fn run_if_present(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    match run(program, args).await {
        Ok(out) => out,
        Err(err) => {
            debug!("{program} unavailable: {err:#}");
            None
        }
    }
}

fn non_empty(bytes: Vec<u8>) -> Option<Vec<u8>> {
    if bytes.is_empty() { None } else { Some(bytes) }
}

mod wsl {
    use super::*;

    const PROBE: &str = "Add-Type -AssemblyName System.Windows.Forms; \
        if ([System.Windows.Forms.Clipboard]::ContainsImage()) { 'image/png' }; \
        if ([System.Windows.Forms.Clipboard]::ContainsText()) { 'text/plain' }";

    /// Base64 rather than raw bytes on stdout: PowerShell's console output is text and would
    /// re-encode a PNG on the way through. Same trick Claude Code uses natively on WSL.
    const IMAGE: &str = "Add-Type -AssemblyName System.Windows.Forms; \
        $i = [System.Windows.Forms.Clipboard]::GetImage(); if ($null -eq $i) { exit 1 }; \
        $ms = New-Object System.IO.MemoryStream; \
        $i.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); \
        [Convert]::ToBase64String($ms.ToArray())";

    const TEXT: &str = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Get-Clipboard -Raw";

    fn powershell() -> PathBuf {
        let on_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("powershell.exe"))
                .find(|candidate| candidate.is_file())
        });
        on_path.unwrap_or_else(|| PathBuf::from(WINDOWS_POWERSHELL))
    }

    async fn run_script(script: &str) -> Result<Option<Vec<u8>>> {
        let exe = powershell();
        run(
            &exe.to_string_lossy(),
            &["-NoProfile", "-NonInteractive", "-Sta", "-Command", script],
        )
        .await
    }

    pub async fn targets() -> Vec<Target> {
        match run_script(PROBE).await {
            Ok(Some(out)) => parse_target_lines(&String::from_utf8_lossy(&out)),
            Ok(None) => Vec::new(),
            Err(err) => {
                debug!("clipboard probe failed: {err:#}");
                Vec::new()
            }
        }
    }

    pub async fn read(target: Target) -> Result<Option<Vec<u8>>> {
        match target {
            Target::Image => {
                let Some(out) = run_script(IMAGE).await? else { return Ok(None) };
                let encoded: Vec<u8> = out.into_iter().filter(|b| !b.is_ascii_whitespace()).collect();
                let png = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .context("powershell returned malformed image data")?;
                Ok(non_empty(png))
            }
            Target::Text => {
                let Some(out) = run_script(TEXT).await? else { return Ok(None) };
                let text = String::from_utf8_lossy(&out).replace("\r\n", "\n");
                Ok(non_empty(text.into_bytes()))
            }
        }
    }
}

mod linux {
    use super::*;

    /// Whichever server is running goes first; the other is tried anyway, since a session can
    /// have both variables set and only one of them right.
    fn tool_order<'a>(wayland: &'a [&'a str], x11: &'a [&'a str]) -> [&'a [&'a str]; 2] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() { [wayland, x11] } else { [x11, wayland] }
    }

    async fn first_answer(wayland: &[&str], x11: &[&str]) -> Option<Vec<u8>> {
        for argv in tool_order(wayland, x11) {
            if let Some(out) = run_if_present(argv[0], &argv[1..]).await {
                return Some(out);
            }
        }
        None
    }

    pub async fn targets() -> Vec<Target> {
        let listed = first_answer(
            &["wl-paste", "--list-types"],
            &["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"],
        )
        .await;
        match listed {
            Some(out) => parse_target_lines(&String::from_utf8_lossy(&out)),
            None => Vec::new(),
        }
    }

    pub async fn read(target: Target) -> Result<Option<Vec<u8>>> {
        let out = match target {
            Target::Image => {
                first_answer(
                    &["wl-paste", "--type", "image/png"],
                    &["xclip", "-selection", "clipboard", "-t", "image/png", "-o"],
                )
                .await
            }
            // No explicit type: text is what both tools default to, and asking for
            // `text/plain` by name fails on a selection that only offers UTF8_STRING.
            Target::Text => {
                first_answer(
                    &["wl-paste", "--no-newline"],
                    &["xclip", "-selection", "clipboard", "-o"],
                )
                .await
            }
        };
        Ok(out.and_then(non_empty))
    }
}

mod macos {
    use super::*;

    const HAS_IMAGE: &str = "the clipboard as «class PNGf»";

    pub async fn targets() -> Vec<Target> {
        let mut targets = Vec::new();
        if run_if_present("osascript", &["-e", HAS_IMAGE]).await.is_some() {
            targets.push(Target::Image);
        }
        if run_if_present("pbpaste", &[]).await.and_then(non_empty).is_some() {
            targets.push(Target::Text);
        }
        targets
    }

    pub async fn read(target: Target) -> Result<Option<Vec<u8>>> {
        match target {
            Target::Image => {
                // osascript prints binary data as hex, so it writes the file itself instead.
                let file = tempfile::NamedTempFile::new().context("failed to create temp file")?;
                let path = file.path().to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
                let script = [
                    "set png_data to (the clipboard as «class PNGf»)".to_string(),
                    format!("set fp to open for access POSIX file \"{path}\" with write permission"),
                    "write png_data to fp".to_string(),
                    "close access fp".to_string(),
                ];
                let mut args = Vec::new();
                for line in &script {
                    args.push("-e");
                    args.push(line);
                }
                if run("osascript", &args).await?.is_none() {
                    return Ok(None);
                }
                let png = std::fs::read(file.path()).context("failed to read clipboard image")?;
                Ok(non_empty(png))
            }
            Target::Text => Ok(run("pbpaste", &[]).await?.and_then(non_empty)),
        }
    }
}

/// Picks the targets the bridge serves out of a tool's own list — `xclip -t TARGETS`,
/// `wl-paste --list-types` and the PowerShell probe all speak in one MIME type per line.
fn parse_target_lines(listing: &str) -> Vec<Target> {
    let mut targets = Vec::new();
    let lines: Vec<&str> = listing.lines().map(str::trim).collect();
    if lines.contains(&"image/png") {
        targets.push(Target::Image);
    }
    if lines.iter().any(|l| l.starts_with("text/plain") || *l == "UTF8_STRING" || *l == "STRING") {
        targets.push(Target::Text);
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_target_lists_map_onto_the_served_kinds() {
        let listing = "TARGETS\nTIMESTAMP\nimage/png\nimage/bmp\n";
        assert_eq!(parse_target_lines(listing), vec![Target::Image]);

        let listing = "UTF8_STRING\nSTRING\ntext/plain;charset=utf-8\n";
        assert_eq!(parse_target_lines(listing), vec![Target::Text]);

        assert_eq!(parse_target_lines("TARGETS\nimage/jpeg\n"), Vec::<Target>::new());
    }

    #[test]
    fn the_powershell_probe_output_parses_with_windows_line_endings() {
        assert_eq!(
            parse_target_lines("image/png\r\ntext/plain\r\n"),
            vec![Target::Image, Target::Text]
        );
    }
}
