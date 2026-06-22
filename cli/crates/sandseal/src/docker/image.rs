use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::{debug, info};

use crate::docker::build;

const REPO_PREFIX: &str = "sandseal-sandbox/agent";

/// Everything that determines which image(s) a sandbox needs.
pub struct ImageSpec<'a> {
    pub agent: &'a str,
    pub project_basename: &'a str,
    pub base_image: &'a str,
    pub uid: u32,
    pub gid: u32,
    pub username: &'a str,
    pub home: &'a str,
    pub dependencies: &'a [String],
    pub setup_script: Option<&'a Path>,
    pub script_dir: &'a Path,
    pub rebuild: bool,
}

impl ImageSpec<'_> {
    fn needs_overlay(&self) -> bool {
        !self.dependencies.is_empty() || self.setup_script.is_some()
    }
}

/// Ensure the images this project needs exist, building only what's missing.
/// Returns the image tag the sandbox should run.
///
/// The base image is project-agnostic and shared across every project with the
/// same inputs (base image, user, agent installs) — so rebuilding it updates all
/// of them at once. A thin per-project overlay is only built when the project
/// defines extra dependencies or a setup hook.
pub fn ensure_images(spec: &ImageSpec) -> Result<String> {
    let base_tag = ensure_base(spec)?;
    if spec.needs_overlay() {
        ensure_overlay(spec, &base_tag)
    } else {
        Ok(base_tag)
    }
}

fn ensure_base(spec: &ImageSpec) -> Result<String> {
    let agents_dir = spec.script_dir.join("agents");

    let mut hasher = Sha1::new();
    hash_str(&mut hasher, spec.base_image);
    hasher.update(spec.uid.to_le_bytes());
    hasher.update(spec.gid.to_le_bytes());
    hash_str(&mut hasher, spec.username);
    hash_str(&mut hasher, spec.home);
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
    build::assemble_base_context(spec.script_dir, ctx.path(), spec.agent)?;

    let mut args = vec![
        ("BASE_IMAGE", spec.base_image.to_string()),
        ("UID", spec.uid.to_string()),
        ("GID", spec.gid.to_string()),
        ("AGENT_USERNAME", spec.username.to_string()),
        ("AGENT_HOME", spec.home.to_string()),
    ];
    if spec.rebuild {
        args.push(("CACHEBUST", cachebust()));
    }
    docker_build(&tag, &agents_dir.join("Dockerfile.base"), ctx.path(), &args)?;
    Ok(tag)
}

fn ensure_overlay(spec: &ImageSpec, base_tag: &str) -> Result<String> {
    let mut hasher = Sha1::new();
    hash_str(&mut hasher, base_tag);
    hash_str(&mut hasher, &spec.dependencies.join(" "));
    if let Some(setup) = spec.setup_script {
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
    build::assemble_overlay_context(ctx.path(), spec.setup_script)?;

    let mut args = vec![
        ("BASE", base_tag.to_string()),
        ("EXTRA_PACKAGES", spec.dependencies.join(" ")),
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

fn docker_build(
    tag: &str,
    dockerfile: &Path,
    context: &Path,
    build_args: &[(&str, String)],
) -> Result<()> {
    let mut cmd = Command::new("docker");
    cmd.arg("build").arg("-t").arg(tag).arg("-f").arg(dockerfile);
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
