//! `sandseal update` — hands the upgrade to the published installer.
//!
//! Deliberately not a second download-and-verify path. `install.sh` already
//! resolves the latest release, checks the download against `SHA256SUMS` and
//! replaces the binary in place; reimplementing that here would mean two ways
//! for a binary to reach the user's machine, and only one of them would stay
//! maintained. This runs exactly what the documented one-liner runs:
//!
//! ```text
//! curl -fsSL https://sandseal.io/install.sh | bash
//! ```

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::cli::UpdateArgs;

const INSTALLER_URL: &str = "https://sandseal.io/install.sh";

pub async fn run(args: UpdateArgs) -> Result<()> {
    println!("  Installed: {}", env!("CARGO_PKG_VERSION"));
    println!("  Fetching {INSTALLER_URL}");

    let script = reqwest::get(INSTALLER_URL)
        .await
        .context("cannot reach sandseal.io")?
        .error_for_status()
        .context("the installer is not available right now")?
        .text()
        .await
        .context("cannot read the installer")?;

    // Fed on stdin rather than through a temp file: nothing has to be writable
    // for this to work, which matters because `sandseal update` may well be run
    // from somewhere with a read-only home. The whole script is already in
    // memory, so bash never executes a half-downloaded file the way a naive
    // `curl | bash` can.
    let mut child = Command::new("bash")
        .arg("-s")
        .arg("--")
        .args(args.version.iter().flat_map(|v| ["--version", v]))
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot run bash — the installer needs it")?;

    child
        .stdin
        .take()
        .context("cannot write to bash")?
        .write_all(script.as_bytes())
        .context("cannot send the installer to bash")?;

    let status = child.wait().context("the installer did not run")?;

    if !status.success() {
        bail!("the installer did not finish successfully");
    }

    Ok(())
}
