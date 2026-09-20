//! The dead sandbox collector.
//!
//! A sandbox is supposed to be torn down by the CLI that started it — on exit, or from the
//! signal handler when the terminal goes away. Neither runs when the process is killed
//! outright or the machine loses power, and what is left behind is not harmless: the
//! container keeps running with an agent inside it, its tmp dir stays on disk, and the
//! backend goes on believing the session is live.
//!
//! So the containers are swept as well as reported. What makes that safe is the instance
//! registry: a sandbox someone is still using holds a lock, and this never touches one that
//! does.
//!
//! The same sweep reclaims the disk, which is the leftover nobody notices until it is
//! measured in hundreds of gigabytes: see `docker::prune`.

use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::docker::prune::{self, PruneReport};
use crate::sandbox::registry::{self, Orphan};

#[derive(Default)]
pub struct SweepReport {
    /// Sandboxes whose CLI is gone, by instance name.
    pub reaped: Vec<String>,
    /// Containers left over from earlier sessions that had already stopped.
    pub removed_stale: usize,
    /// Images and volumes nothing has wanted for long enough to reclaim.
    pub pruned: PruneReport,
}

impl SweepReport {
    pub fn is_empty(&self) -> bool {
        self.reaped.is_empty() && self.removed_stale == 0 && self.pruned.is_empty()
    }

    /// The one line `sandseal start` prints, when there is anything to say.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.reaped.is_empty() {
            parts.push(format!("{} abandoned sandbox(es)", self.reaped.len()));
        }
        if self.removed_stale > 0 {
            parts.push(format!("{} stopped container(s)", self.removed_stale));
        }
        if !self.pruned.images.is_empty() {
            parts.push(format!("{} unused image(s)", self.pruned.images.len()));
        }
        if !self.pruned.volumes.is_empty() {
            parts.push(format!("{} unused volume(s)", self.pruned.volumes.len()));
        }
        format!("Cleaned up {}.", join_with_and(&parts))
    }
}

/// Puts a list into a sentence: "a", "a and b", "a, b and c".
fn join_with_and(parts: &[String]) -> String {
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Reaps everything nobody is driving. `dry_run` reports without touching anything.
///
/// `keep_days` is how long an unused image or volume is kept before its disk is reclaimed;
/// 0 leaves them alone entirely.
pub async fn sweep(keep_days: u64, dry_run: bool) -> SweepReport {
    let mut report = SweepReport::default();

    for orphan in registry::orphans() {
        report.reaped.push(orphan.record.instance_name.clone());
        if dry_run {
            continue;
        }
        reap(orphan).await;
    }

    report.removed_stale = if dry_run { count_stale() } else { remove_stale() };
    // After the containers, never before: an image or volume still attached to one of them is
    // skipped, and reaping first is what makes the ones we just freed collectable.
    report.pruned = prune::sweep(keep_days, dry_run);
    report
}

async fn reap(orphan: Orphan) {
    let record = orphan.record.clone();
    debug!("reaping abandoned sandbox {}", record.instance_name);

    take_down(&record.instance_name);
    remove_tmp_dir(&record.tmp_dir);

    // Ends the session server-side, which is what revokes the memory credential still sitting
    // in the container we just removed. Best-effort, like every other call to it.
    if let Some(session_id) = &record.session_id {
        crate::memory::session::close(record.api_url.as_deref(), session_id).await;
    }

    orphan.forget();
}

/// Stops and removes an instance's containers.
fn take_down(instance_name: &str) {
    // By project name alone, with no `-f`: the compose override lives in the tmp dir this
    // sweep is about to delete, and after a reboot it may be gone already.
    let _ = Command::new("docker")
        .args(["compose", "-p", instance_name, "down", "--remove-orphans"])
        .output();

    // Belt and braces. Compose that cannot resolve the project reports success and leaves
    // the container running, which is the exact failure this whole file exists to fix.
    for id in container_ids(&[
        "--filter",
        &format!("label=sandseal.instance_name={instance_name}"),
    ]) {
        let _ = Command::new("docker").args(["rm", "-f", "-v", &id]).output();
    }
}

fn remove_tmp_dir(tmp_dir: &Path) {
    if !tmp_dir.exists() {
        return;
    }
    // Only ever a directory this CLI created under ~/.sandseal/tmp. A record pointing
    // anywhere else is not one we wrote.
    let Some(home) = dirs::home_dir() else { return };
    if !tmp_dir.starts_with(home.join(".sandseal/tmp")) {
        debug!("refusing to remove tmp dir outside ~/.sandseal/tmp: {}", tmp_dir.display());
        return;
    }
    if let Err(err) = std::fs::remove_dir_all(tmp_dir) {
        debug!("could not remove {}: {err}", tmp_dir.display());
    }
}

/// Containers from sessions that already ended. Docker keeps them until someone asks.
fn stale_container_ids() -> Vec<String> {
    container_ids(&[
        "--filter",
        "label=sandseal.project_name",
        // Both are terminal states, and the filters are OR'd. A running container is never
        // in this list, whoever started it — the collector does not judge liveness by state.
        "--filter",
        "status=exited",
        "--filter",
        "status=dead",
    ])
}

fn count_stale() -> usize {
    stale_container_ids().len()
}

fn remove_stale() -> usize {
    let ids = stale_container_ids();
    let mut removed = 0;
    for id in &ids {
        // `-v` takes any anonymous volume the container declared with it. Named volumes —
        // the agent home, the apt cache — are not anonymous and are left alone.
        if Command::new("docker").args(["rm", "-v", id]).output().is_ok_and(|o| o.status.success())
        {
            removed += 1;
        }
    }
    removed
}

fn container_ids(filters: &[&str]) -> Vec<String> {
    let mut args = vec!["ps", "-a"];
    args.extend_from_slice(filters);
    args.extend_from_slice(&["--format", "{{.ID}}"]);

    let Ok(output) = Command::new("docker").args(&args).output() else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_sweep_says_nothing() {
        assert!(SweepReport::default().is_empty());
    }

    #[test]
    fn the_summary_names_both_kinds_of_leftover() {
        let report = SweepReport {
            reaped: vec!["sandseal-sandbox-demo-ab12".into()],
            removed_stale: 4,
            ..Default::default()
        };
        assert_eq!(report.summary(), "Cleaned up 1 abandoned sandbox(es) and 4 stopped container(s).");
    }

    #[test]
    fn the_summary_leaves_out_what_did_not_happen() {
        let report = SweepReport { removed_stale: 2, ..Default::default() };
        assert_eq!(report.summary(), "Cleaned up 2 stopped container(s).");
    }

    #[test]
    fn the_summary_counts_the_disk_it_reclaimed() {
        let report = SweepReport {
            removed_stale: 1,
            pruned: PruneReport {
                images: vec!["sandseal-sandbox/agent-claude:base-old".into()],
                volumes: vec!["a".into(), "b".into()],
            },
            ..Default::default()
        };
        assert_eq!(
            report.summary(),
            "Cleaned up 1 stopped container(s), 1 unused image(s) and 2 unused volume(s).",
        );
    }

    #[test]
    fn a_sweep_that_reclaimed_only_disk_is_still_worth_reporting() {
        let report = SweepReport {
            pruned: PruneReport { images: Vec::new(), volumes: vec!["v".into()] },
            ..Default::default()
        };
        assert!(!report.is_empty());
        assert_eq!(report.summary(), "Cleaned up 1 unused volume(s).");
    }
}
