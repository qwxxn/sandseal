use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use crate::config::schema::ScriptHook;
use crate::docker::runtime;
use crate::sandbox::registry::InstanceClaim;

/// State needed for cleanup on exit.
pub struct CleanupGuard {
    pub compose_cmd: Vec<String>,
    pub cleanup_hooks: Vec<ScriptHook>,
    pub project_dir: PathBuf,
    pub tmp_dir: PathBuf,
    /// The backend session to close, which is what revokes the sandbox's memory credential.
    session: Option<(Option<String>, String)>,
    /// Holding it is what tells the collector this sandbox is alive; dropping it is what
    /// makes a crashed CLI reapable.
    claim: Option<InstanceClaim>,
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
            session: None,
            claim: None,
            done: false,
        }
    }

    /// Close this session on the way out. Set for every sandbox that reached the backend.
    pub fn closing_session(&mut self, api_url: Option<String>, session_id: String) {
        self.session = Some((api_url, session_id));
    }

    pub fn holding(&mut self, claim: InstanceClaim) {
        self.claim = Some(claim);
    }

    /// Run the full cleanup sequence: compose down → cleanup hooks → remove tmp dir →
    /// close the session → drop the instance record.
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

        // Phase 4: close the backend session. Here rather than at the end of `start` because
        // the common way a sandbox ends is the signal handler, which never returns there.
        if let Some((api_url, session_id)) = self.session.take() {
            close_session(api_url, session_id);
        }

        // Phase 5: the record is the collector's to-do list, and this sandbox is done.
        if let Some(claim) = self.claim.take() {
            claim.remove();
        }

        info!("sandbox destroyed");
    }
}

/// One async call from a synchronous cleanup that may run inside a runtime (a normal exit),
/// outside one (the signal handler), or during a drop. A thread with its own runtime is the
/// only shape that is safe in all three — building one inline panics when a runtime is
/// already on the thread.
fn close_session(api_url: Option<String>, session_id: String) {
    let worker = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => {
                debug!("cannot close session {session_id}: {err}");
                return;
            }
        };
        runtime.block_on(crate::memory::session::close(api_url.as_deref(), &session_id));
    });

    // Waited on: the process is about to exit, and a detached request would never be sent.
    if worker.join().is_err() {
        debug!("session close thread panicked");
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Register the handler that triggers cleanup when the CLI is asked to stop.
///
/// With the `termination` feature this covers SIGHUP as well as SIGINT and SIGTERM, and
/// SIGHUP is the one that matters most: it is what closing the terminal sends, which used to
/// kill the CLI outright and leave the container running with nobody attached.
pub fn register_signal_handler(guard: Arc<Mutex<CleanupGuard>>) {
    ctrlc::set_handler(move || {
        let mut guard = guard.lock().unwrap();
        guard.cleanup();
        std::process::exit(130);
    })
    .expect("failed to register signal handler");
}
