use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use tracing::{debug, info};

use crate::docker::build;

const REPO_PREFIX: &str = "sandseal-sandbox/agent";

/// Extra apt packages and a setup hook, baked into one image layer.
#[derive(Default)]
pub struct Payload<'a> {
    pub dependencies: &'a [String],
    pub setup_script: Option<&'a Path>,
}

impl Payload<'_> {
    fn is_empty(&self) -> bool {
        self.dependencies.is_empty() && self.setup_script.is_none()
    }
}

/// Everything that determines which image(s) a sandbox needs.
pub struct ImageSpec<'a> {
    pub agent: &'a str,
    pub project_basename: &'a str,
    pub base_image: &'a str,
    /// Replaces the Ubuntu archive the base image points at, when set.
    pub apt_mirror: Option<&'a str>,
    pub uid: u32,
    pub gid: u32,
    pub username: &'a str,
    pub home: &'a str,
    /// Machine-wide, from settings this project did not touch. Goes into the base image,
    /// so it is installed once for every project instead of once per project.
    pub shared: Payload<'a>,
    /// What this project adds on top. Goes into its own overlay.
    pub project: Payload<'a>,
    pub script_dir: &'a Path,
    pub rebuild: bool,
}

/// Ensure the images this project needs exist, building only what's missing.
/// Returns the image tag the sandbox should run.
///
/// The base image is project-agnostic and shared across every project with the
/// same inputs (base image, user, agent installs, machine-wide payload) — so rebuilding
/// it updates all of them at once. A thin per-project overlay is only built when the
/// project itself adds dependencies or a setup hook.
pub fn ensure_images(spec: &ImageSpec) -> Result<String> {
    let base_tag = ensure_base(spec)?;
    if spec.project.is_empty() {
        Ok(base_tag)
    } else {
        ensure_overlay(spec, &base_tag)
    }
}

fn ensure_base(spec: &ImageSpec) -> Result<String> {
    let agents_dir = spec.script_dir.join("agents");

    let mut hasher = Sha1::new();
    hash_str(&mut hasher, spec.base_image);
    hash_str(&mut hasher, spec.apt_mirror.unwrap_or_default());
    hasher.update(spec.uid.to_le_bytes());
    hasher.update(spec.gid.to_le_bytes());
    hash_str(&mut hasher, spec.username);
    hash_str(&mut hasher, spec.home);
    hash_str(&mut hasher, &spec.shared.dependencies.join(" "));
    if let Some(setup) = spec.shared.setup_script {
        hash_path(&mut hasher, setup)?;
    }
    hash_path(&mut hasher, &agents_dir.join("Dockerfile.base"))?;
    hash_path(&mut hasher, &agents_dir.join("entrypoint.sh"))?;
    hash_path(&mut hasher, &agents_dir.join("apt-wrapper.sh"))?;
    hash_path(&mut hasher, &agents_dir.join(spec.agent))?;
    let hash = format!("{:x}", hasher.finalize());
    let tag = format!("{REPO_PREFIX}-{}:base-{}", spec.agent, &hash[..12]);

    if image_exists(&tag) && !spec.rebuild {
        debug!("base image up to date: {tag}");
        return Ok(tag);
    }

    info!("building base image {tag}");
    let ctx = make_context_dir()?;
    build::assemble_base_context(spec.script_dir, ctx.path(), spec.agent, spec.shared.setup_script)?;

    let mut args = vec![
        ("BASE_IMAGE", spec.base_image.to_string()),
        ("UID", spec.uid.to_string()),
        ("GID", spec.gid.to_string()),
        ("AGENT_USERNAME", spec.username.to_string()),
        ("AGENT_HOME", spec.home.to_string()),
        ("SHARED_PACKAGES", spec.shared.dependencies.join(" ")),
        ("APT_MIRROR", spec.apt_mirror.unwrap_or_default().to_string()),
    ];
    if spec.rebuild {
        args.push(("CACHEBUST", cachebust()));
    }
    docker_build(&tag, &agents_dir.join("Dockerfile.base"), ctx.path(), &args)?;
    Ok(tag)
}

fn ensure_overlay(spec: &ImageSpec, base_tag: &str) -> Result<String> {
    // Keyed on the base image's ID, not its tag: the tag is a hash of the base's *inputs*,
    // so a rebuilt base keeps it. Hashing the tag would leave every existing overlay looking
    // current while it sits on layers that are gone.
    let base_id = image_id(base_tag).unwrap_or_else(|| base_tag.to_string());

    let mut hasher = Sha1::new();
    hash_str(&mut hasher, &base_id);
    hash_str(&mut hasher, &spec.project.dependencies.join(" "));
    if let Some(setup) = spec.project.setup_script {
        hash_path(&mut hasher, setup)?;
    }
    let hash = format!("{:x}", hasher.finalize());
    let tag = format!("{REPO_PREFIX}-{}:{}-{}", spec.agent, spec.project_basename, &hash[..8]);

    if image_exists(&tag) && !spec.rebuild {
        debug!("overlay image up to date: {tag}");
        return Ok(tag);
    }

    info!("building overlay image {tag}");
    let ctx = make_context_dir()?;
    build::assemble_setup_scripts(ctx.path(), spec.project.setup_script)?;

    let mut args = vec![
        ("BASE", base_tag.to_string()),
        ("EXTRA_PACKAGES", spec.project.dependencies.join(" ")),
    ];
    if spec.rebuild {
        args.push(("CACHEBUST", cachebust()));
    }
    docker_build(
        &tag,
        &spec.script_dir.join("agents/Dockerfile.overlay"),
        ctx.path(),
        &args,
    )?;
    Ok(tag)
}

/// Remove a project's overlay images, leaving the shared base image intact.
pub fn remove_project_overlays(agent: &str, project_basename: &str) {
    let repo = format!("{REPO_PREFIX}-{agent}");
    let prefix = format!("{project_basename}-");
    for image in list_repo_images(&repo) {
        let tag = image.rsplit_once(':').map(|(_, t)| t).unwrap_or("");
        // base images are tagged `base-<hash>`; only overlays carry the project basename
        if tag.starts_with(&prefix) {
            let _ = Command::new("docker").args(["rmi", &image]).output();
        }
    }
}

fn list_repo_images(repo: &str) -> Vec<String> {
    Command::new("docker")
        .args(["images", "--format", "{{.Repository}}:{{.Tag}}", repo])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn make_context_dir() -> Result<tempfile::TempDir> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let base = home.join(".sandseal/tmp");
    fs::create_dir_all(&base)?;
    tempfile::tempdir_in(&base).context("failed to create build context dir")
}

fn cachebust() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "1".to_string())
}

fn image_exists(tag: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The image's content ID, which changes whenever it is rebuilt.
fn image_id(tag: &str) -> Option<String> {
    let output = Command::new("docker")
        .args(["image", "inspect", "-f", "{{.Id}}", tag])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Whether this machine can build with BuildKit, which the apt cache mounts require.
///
/// Docker only reaches BuildKit through the buildx component; without it, `DOCKER_BUILDKIT=1`
/// is a hard error rather than a fallback, so the answer decides which Dockerfile we hand over.
fn buildkit_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        // An explicit DOCKER_BUILDKIT=0 in the environment is the user turning it off.
        if std::env::var("DOCKER_BUILDKIT").is_ok_and(|v| v == "0") {
            return false;
        }
        let ok = Command::new("docker")
            .args(["buildx", "version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            tracing::warn!(
                "docker buildx is missing — building without the apt cache, so every rebuild \
                 re-downloads each package. Install the buildx component (Debian/Ubuntu: \
                 `apt-get install docker-buildx-plugin`, macOS: bundled with Docker Desktop) \
                 to cut rebuild times."
            );
        }
        ok
    })
}

/// Drop the `--mount=type=cache` flags so the legacy builder can parse the Dockerfile.
///
/// Lines left holding nothing but a line continuation are dropped whole; the `RUN` itself keeps
/// its trailing backslash and joins with the command below it.
fn strip_cache_mounts(dockerfile: &str) -> String {
    let mut out = String::with_capacity(dockerfile.len());
    for line in dockerfile.lines() {
        let kept: Vec<&str> = line
            .split_whitespace()
            .filter(|token| !token.starts_with("--mount="))
            .collect();
        if kept.is_empty() || (kept == ["\\"] && line.contains("--mount=")) {
            continue;
        }
        if kept.len() == line.split_whitespace().count() {
            out.push_str(line);
        } else {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&indent);
            out.push_str(&kept.join(" "));
        }
        out.push('\n');
    }
    out
}

fn docker_build(
    tag: &str,
    dockerfile: &Path,
    context: &Path,
    build_args: &[(&str, String)],
) -> Result<()> {
    // The Dockerfiles mount apt caches with `RUN --mount=type=cache`, which only BuildKit
    // understands. Without it, build the same image from a stripped copy instead of failing.
    let (dockerfile, buildkit) = if buildkit_available() {
        (dockerfile.to_path_buf(), true)
    } else {
        let source = fs::read_to_string(dockerfile)
            .with_context(|| format!("failed to read: {}", dockerfile.display()))?;
        let stripped = context.join("Dockerfile.nocachemount");
        fs::write(&stripped, strip_cache_mounts(&source))
            .context("failed to write legacy-builder Dockerfile")?;
        (stripped, false)
    };

    let mut cmd = Command::new("docker");
    cmd.env("DOCKER_BUILDKIT", if buildkit { "1" } else { "0" });
    cmd.arg("build").arg("-t").arg(tag).arg("-f").arg(&dockerfile);
    for (key, val) in build_args {
        cmd.arg("--build-arg").arg(format!("{key}={val}"));
    }
    cmd.arg(context)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd.status().context("failed to run docker build")?;
    if !status.success() {
        bail!("docker build failed (exit {})", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn hash_str(hasher: &mut Sha1, s: &str) {
    hasher.update(s.as_bytes());
    hasher.update([0]);
}

/// Hash a file's bytes, or a directory's contents recursively (sorted for determinism).
fn hash_path(hasher: &mut Sha1, path: &Path) -> Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .with_context(|| format!("failed to read dir: {}", path.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::result::Result<_, _>>()?;
        entries.sort();
        for entry in entries {
            let name = entry.file_name().unwrap_or_default().to_string_lossy().to_string();
            hash_str(hasher, &name);
            hash_path(hasher, &entry)?;
        }
    } else if path.is_file() {
        let bytes = fs::read(path).with_context(|| format!("failed to read: {}", path.display()))?;
        hasher.update(&bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_MOUNTS: &str = r#"FROM ubuntu:24.04
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update && apt-get install -y curl
RUN echo done
"#;

    #[test]
    fn strips_mount_flags_but_keeps_the_command() {
        let out = strip_cache_mounts(WITH_MOUNTS);
        assert!(!out.contains("--mount="));
        assert!(out.contains("apt-get update && apt-get install -y curl"));
        assert!(out.contains("RUN echo done"));
    }

    #[test]
    fn leaves_the_run_joined_to_its_command() {
        let out = strip_cache_mounts(WITH_MOUNTS);
        // `RUN \` must survive so the continuation still binds to it, and the line that held
        // nothing but the second mount must not leave a dangling backslash behind.
        assert!(out.contains("RUN \\\n    apt-get update"), "unexpected output:\n{out}");
    }

    #[test]
    fn leaves_a_dockerfile_without_mounts_untouched() {
        let plain = "FROM ubuntu:24.04\nRUN apt-get update \\\n    && apt-get install -y curl\n";
        assert_eq!(strip_cache_mounts(plain), plain);
    }
}
