use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use crate::config::schema::ScriptHook;
use crate::docker::runtime;

/// State needed for cleanup on exit.
pub struct CleanupGuard {
    pub compose_cmd: Vec<String>,
    pub cleanup_hooks: Vec<ScriptHook>,
    pub project_dir: PathBuf,
    pub tmp_dir: PathBuf,
    done: bool,
}

impl CleanupGuard {
    pub fn new(
        compose_cmd: Vec<String>,
        cleanup_hooks: Vec<ScriptHook>,
        project_dir: PathBuf,
        tmp_dir: PathBuf,
    ) -> Self {
        Self {
            compose_cmd,
            cleanup_hooks,
            project_dir,
            tmp_dir,
            done: false,
        }
    }

    /// Run the full cleanup sequence: compose down → cleanup hooks → remove tmp dir.
    pub fn cleanup(&mut self) {
        if self.done {
            return;
        }
        self.done = true;

        // Phase 1: compose down
        info!("stopping sandbox...");
        if let Err(e) = runtime::compose_down(&self.compose_cmd) {
            error!("compose down failed: {e}");
        }

        // Phase 2: cleanup host hooks
        if !self.cleanup_hooks.is_empty() {
            super::hooks::run_cleanup_host_hooks(&self.cleanup_hooks, &self.project_dir);
        }

        // Phase 3: remove tmp dir
        if self.tmp_dir.exists() {
            debug!("removing tmp dir: {}", self.tmp_dir.display());
            if let Err(e) = std::fs::remove_dir_all(&self.tmp_dir) {
                error!("failed to remove tmp dir: {e}");
            }
        }

        info!("sandbox destroyed");
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Register SIGINT/SIGTERM handler that triggers cleanup.
pub fn register_signal_handler(guard: Arc<Mutex<CleanupGuard>>) {
    ctrlc::set_handler(move || {
        let mut guard = guard.lock().unwrap();
        guard.cleanup();
        std::process::exit(130);
    })
    .expect("failed to register signal handler");
}
