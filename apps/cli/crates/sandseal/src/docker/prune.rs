//! Reclaiming the disk a sandbox leaves behind.
//!
//! Containers are torn down when a session ends, but the two things that actually take up
//! space are not: the images each build leaves behind, and the volumes older layouts created
//! per instance. Nothing ever asked for those again and nothing ever removed them, so a few
//! months of daily use is measured in hundreds of gigabytes.
//!
//! What makes removal safe here is that it is never forced. Docker refuses to remove an image
//! or a volume something still references, so the worst a mistake can cost is a rebuild — and
//! even that is avoided by tracking when the CLI last resolved an image (`record_image_use`)
//! rather than guessing from its build date. An image in daily use keeps a fresh marker even
//! though its layers are months old; one whose inputs changed is never touched again and ages
//! out.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::debug;

use crate::docker::compose::SHARED_VOLUMES;

/// Repository namespace every image this CLI builds lives under.
const IMAGE_NAMESPACE: &str = "sandseal-sandbox/";

/// Prefix of the volumes compose created per instance before the shared ones were pinned to
/// fixed names. Each one is a full agent home — a couple of gigabytes — and because the
/// compose project name carried a random suffix, no later start ever mounts one again.
const VOLUME_PREFIX: &str = "sandseal-sandbox-";

/// How long an unused image or volume is kept before it is reclaimed.
pub const DEFAULT_KEEP_DAYS: u64 = 14;

#[derive(Default)]
pub struct PruneReport {
    /// Image tags removed, or that would be.
    pub images: Vec<String>,
    /// Volume names removed, or that would be.
    pub volumes: Vec<String>,
}

impl PruneReport {
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.volumes.is_empty()
    }
}

/// Reclaim the images and volumes nothing has wanted for `keep_days`.
///
/// `keep_days == 0` turns the whole thing off. `dry_run` reports without touching anything.
pub fn sweep(keep_days: u64, dry_run: bool) -> PruneReport {
    let mut report = PruneReport::default();
    if keep_days == 0 {
        debug!("gc.keepDays is 0, leaving unused images and volumes alone");
        return report;
    }

    let cutoff = match now().checked_sub(keep_days * 86_400) {
        Some(cutoff) => cutoff,
        None => return report,
    };

    report.images = stale_images(cutoff);
    report.volumes = stale_volumes(cutoff);

    if !dry_run {
        for tag in &report.images {
            // Never forced: docker declines to remove an image a container still references,
            // which is the backstop behind every rule above.
            let _ = Command::new("docker").args(["rmi", tag]).output();
        }
        for name in &report.volumes {
            let _ = Command::new("docker").args(["volume", "rm", name]).output();
        }
        forget_vanished_images();
    }

    report
}

/// Images in our namespace that no container references and that nothing has resolved since
/// the cutoff — superseded base and overlay tags, and whatever an older naming layout left.
///
/// Untagged images are included when they carry our label: `--rebuild` writes new content
/// under the same content-addressed tag, and the image it replaces is left dangling.
fn stale_images(cutoff: u64) -> Vec<String> {
    let in_use = images_in_use();
    let mut stale = Vec::new();

    for image in list_images() {
        if in_use.contains(&image.id) || in_use.contains(&image.reference) {
            continue;
        }
        if last_used(&image.id).unwrap_or(image.created) >= cutoff {
            continue;
        }
        stale.push(image.reference);
    }

    stale
}

/// Volumes from the per-instance layout that no container mounts any more.
///
/// The two machine-wide volumes are excluded by name: they are shared by design, so between
/// sessions they look exactly like the garbage this collects, and removing one would drop
/// every agent login and installed tool on the machine.
fn stale_volumes(cutoff: u64) -> Vec<String> {
    let shared: HashSet<&str> = SHARED_VOLUMES.iter().map(|(_, name)| *name).collect();

    let names: Vec<String> = dangling_volumes()
        .into_iter()
        .filter(|name| name.starts_with(VOLUME_PREFIX) && !shared.contains(name.as_str()))
        .collect();

    volume_ages(&names)
        .into_iter()
        .filter(|(_, created)| *created < cutoff)
        .map(|(name, _)| name)
        .collect()
}

struct ImageRow {
    /// Full `sha256:…` id, so it compares against what a container reports.
    id: String,
    /// What `docker rmi` is given: `repo:tag`, or the id when the image is untagged.
    reference: String,
    created: u64,
}

fn list_images() -> Vec<ImageRow> {
    let mut rows = Vec::new();
    let format = "{{.ID}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedAt}}";
    let tagged = vec!["--filter".to_string(), format!("reference={IMAGE_NAMESPACE}*")];
    // Our own images that lost their tag to a rebuild. The label is what makes them ours to
    // remove — an untagged image is otherwise unattributable.
    let untagged = vec![
        "--filter".to_string(),
        "dangling=true".to_string(),
        "--filter".to_string(),
        "label=sandseal.image".to_string(),
    ];

    for filter in [tagged, untagged] {
        let mut args =
            vec!["images".into(), "--no-trunc".into(), "--format".into(), format.to_string()];
        args.extend(filter);

        let Ok(output) = Command::new("docker").args(&args).output() else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut parts = line.split('\t');
            let (Some(id), Some(reference), Some(created)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(created) = parse_timestamp(created) else { continue };
            // A dangling image formats as `<none>:<none>`; remove those by id.
            let reference = if reference.starts_with("<none>") { id } else { reference };
            rows.push(ImageRow {
                id: id.to_string(),
                reference: reference.to_string(),
                created,
            });
        }
    }

    rows.sort_by(|a, b| a.reference.cmp(&b.reference));
    rows.dedup_by(|a, b| a.reference == b.reference);
    rows
}

/// Every image id and image reference some container — running or not — is built on.
fn images_in_use() -> HashSet<String> {
    let mut in_use = HashSet::new();

    let Ok(list) = Command::new("docker").args(["ps", "-aq"]).output() else {
        return in_use;
    };
    let ids: Vec<String> = String::from_utf8_lossy(&list.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return in_use;
    }

    let mut args = vec!["inspect".to_string(), "--format".to_string(), "{{.Image}}\t{{.Config.Image}}".to_string()];
    args.extend(ids);
    let Ok(output) = Command::new("docker").args(&args).output() else {
        return in_use;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        for field in line.split('\t') {
            let field = field.trim();
            if !field.is_empty() {
                in_use.insert(field.to_string());
            }
        }
    }

    in_use
}

fn dangling_volumes() -> Vec<String> {
    let Ok(output) = Command::new("docker")
        .args(["volume", "ls", "--filter", "dangling=true", "--format", "{{.Name}}"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Creation times, inspected in batches so a machine with hundreds of leftovers stays within
/// the command line limit.
fn volume_ages(names: &[String]) -> Vec<(String, u64)> {
    let mut ages = Vec::new();
    for chunk in names.chunks(100) {
        let mut args = vec![
            "volume".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{.Name}}\t{{.CreatedAt}}".to_string(),
        ];
        args.extend_from_slice(chunk);
        let Ok(output) = Command::new("docker").args(&args).output() else { continue };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut parts = line.split('\t');
            let (Some(name), Some(created)) = (parts.next(), parts.next()) else { continue };
            if let Some(created) = parse_timestamp(created) {
                ages.push((name.to_string(), created));
            }
        }
    }
    ages
}

/// Note that an image is still wanted, so the collector leaves it alone.
///
/// Called whenever `sandseal start` resolves one, built or reused. The marker's mtime is the
/// record; there is nothing to read, parse or keep consistent, and two CLIs touching the same
/// image at once cannot corrupt it.
pub fn record_image_use(image_id: &str) {
    let Some(path) = marker_path(image_id) else { return };
    let Some(dir) = path.parent() else { return };
    if let Err(err) = fs::create_dir_all(dir) {
        debug!("could not create {}: {err}", dir.display());
        return;
    }
    // Rewriting the file is what moves its mtime to now; File::create alone would not on a
    // file that already exists with the same (empty) contents on some filesystems.
    if let Err(err) = fs::write(&path, now().to_string()) {
        debug!("could not record image use in {}: {err}", path.display());
    }
}

fn last_used(image_id: &str) -> Option<u64> {
    let path = marker_path(image_id)?;
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// Markers for images that are no longer on the machine. Cheap to leave, tidier to drop.
fn forget_vanished_images() {
    let Some(dir) = marker_dir() else { return };
    let Ok(entries) = fs::read_dir(&dir) else { return };

    let present: HashSet<String> = list_all_image_ids();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !present.contains(&format!("sha256:{name}")) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn list_all_image_ids() -> HashSet<String> {
    let Ok(output) = Command::new("docker")
        .args(["images", "--no-trunc", "-aq"])
        .output()
    else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn marker_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".sandseal/image-use"))
}

fn marker_path(image_id: &str) -> Option<PathBuf> {
    let hex = image_id.strip_prefix("sha256:").unwrap_or(image_id);
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(marker_dir()?.join(hex))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Unix seconds from the two shapes docker prints a time in: RFC 3339 from `inspect`
/// (`2026-06-22T15:01:51+02:00`) and Go's own format from `images` (`2026-09-20 02:39:24
/// +0200 CEST`).
///
/// Written out rather than pulled in with a date crate: this is the only date the CLI parses,
/// and it is one the same program asked for a moment earlier.
fn parse_timestamp(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.len() < 19 {
        return None;
    }
    let (date, rest) = raw.split_at(10);
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;

    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    let mut time = rest.get(..8)?.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next()?.parse().ok()?;

    let civil = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    let seconds = civil - utc_offset(rest.get(8..)?);
    (seconds >= 0).then_some(seconds as u64)
}

/// The zone suffix, in seconds east of UTC. An unreadable one is treated as UTC: being a few
/// hours out never decides anything, because the cutoff is measured in days.
fn utc_offset(tail: &str) -> i64 {
    let tail = tail.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    let tail = tail.trim_start();
    let (sign, rest) = match tail.as_bytes().first() {
        Some(b'+') => (1, &tail[1..]),
        Some(b'-') => (-1, &tail[1..]),
        _ => return 0,
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).take(4).collect();
    if digits.len() < 4 {
        return 0;
    }
    let hours: i64 = digits[..2].parse().unwrap_or(0);
    let minutes: i64 = digits[2..4].parse().unwrap_or(0);
    sign * (hours * 3600 + minutes * 60)
}

/// Days between the civil date and 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_rfc3339_form_docker_inspect_prints() {
        // 2026-06-22T15:01:51+02:00 == 2026-06-22T13:01:51Z
        assert_eq!(parse_timestamp("2026-06-22T15:01:51+02:00"), Some(1_782_133_311));
    }

    #[test]
    fn reads_the_form_docker_images_prints() {
        assert_eq!(
            parse_timestamp("2026-09-20 02:39:24 +0200 CEST"),
            parse_timestamp("2026-09-20T02:39:24+02:00"),
        );
    }

    #[test]
    fn reads_fractional_seconds_and_zulu() {
        let fractional = parse_timestamp("2026-09-18T00:08:44.240508526+02:00").unwrap();
        assert_eq!(fractional, parse_timestamp("2026-09-18T00:08:44+02:00").unwrap());
        assert_eq!(parse_timestamp("1970-01-02T00:00:00Z"), Some(86_400));
    }

    #[test]
    fn the_epoch_and_a_leap_day_land_where_they_should() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2024-02-29T00:00:00Z"), Some(1_709_164_800));
    }

    #[test]
    fn nonsense_is_not_read_as_a_date() {
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("N/A"), None);
        assert_eq!(parse_timestamp("2026-09-20"), None);
        assert_eq!(parse_timestamp("not-a-date at all here"), None);
    }

    #[test]
    fn a_marker_is_named_after_the_image_and_nothing_else() {
        let home = dirs::home_dir().unwrap();
        let id = "sha256:df5c5020eb1899063226ef5089ea4a605717ce3d366fafcc60a4c4452f202ffd";
        assert_eq!(
            marker_path(id).unwrap(),
            home.join(".sandseal/image-use")
                .join("df5c5020eb1899063226ef5089ea4a605717ce3d366fafcc60a4c4452f202ffd")
        );
        // Anything that is not a plain digest cannot become a path.
        assert_eq!(marker_path("sha256:../../etc/passwd"), None);
        assert_eq!(marker_path(""), None);
    }

    #[test]
    fn the_shared_volumes_are_never_collected() {
        let shared: HashSet<&str> = SHARED_VOLUMES.iter().map(|(_, name)| *name).collect();
        // Both carry the prefix the collector sweeps, which is exactly why they are excluded
        // by name — between sessions they are dangling like everything else.
        for (_, name) in SHARED_VOLUMES {
            assert!(name.starts_with(VOLUME_PREFIX));
            assert!(shared.contains(name));
        }
        assert!(!shared.contains("sandseal-sandbox-work-9535b69f_sandseal-agent-home"));
    }

    #[test]
    fn a_recorded_image_reads_back_as_used_just_now() {
        // A digest no image has, so the round trip cannot collide with a real marker and the
        // collector would drop it as vanished anyway.
        let id = format!("sha256:{}", "fe".repeat(32));
        assert_eq!(last_used(&id), None, "test digest already has a marker");

        record_image_use(&id);
        let recorded = last_used(&id).expect("marker was not written");
        assert!(now() - recorded < 60, "marker mtime is not the time of use");

        let _ = fs::remove_file(marker_path(&id).unwrap());
    }

    #[test]
    fn keeping_nothing_means_collecting_nothing() {
        let report = sweep(0, true);
        assert!(report.is_empty());
    }
}
