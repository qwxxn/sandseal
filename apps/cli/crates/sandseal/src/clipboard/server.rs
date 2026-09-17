//! The host side: a unix socket in the instance tmp dir, answering one request per connection.
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::host::HostClipboard;
use super::{Request, Response, Target};

/// A client that connects and says nothing is not allowed to hold a connection open forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the clipboard comes from. The host implementation shells out to platform tools;
/// tests substitute a fixed one.
pub trait Source: Send + Sync + 'static {
    fn targets(&self) -> impl Future<Output = Vec<Target>> + Send;
    fn read(&self, target: Target) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;
}

/// A bridge serving the host clipboard, alive until the handle is dropped or aborted.
pub struct Bridge {
    pub socket: PathBuf,
    task: JoinHandle<()>,
}

impl Bridge {
    pub fn stop(&self) {
        self.task.abort();
    }
}

/// Starts serving the host clipboard for one sandbox, or explains why it cannot.
///
/// Never fatal: a sandbox without image paste is still a sandbox, so the platform having no
/// clipboard tool the CLI knows, or the socket failing to bind, degrades to "no bridge".
pub fn start(tmp_dir: &Path) -> Option<Bridge> {
    let Some(host) = HostClipboard::detect() else {
        debug!("clipboard bridge unavailable: no clipboard reader for this platform");
        return None;
    };
    debug!("clipboard bridge reads the {:?} clipboard", host.platform());

    let socket = super::host_socket(tmp_dir);
    match bind(&socket) {
        Ok(listener) => Some(Bridge { socket, task: spawn(listener, host) }),
        Err(err) => {
            warn!("clipboard bridge disabled, image paste will not work in the sandbox: {err:#}");
            None
        }
    }
}

/// Binds the socket, owner-only: the sandbox runs as the same uid, and nobody else on the
/// machine gets to read the clipboard through it.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("cannot replace {}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("cannot listen on {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", path.display()))?;
    Ok(listener)
}

pub fn spawn<S: Source>(listener: UnixListener, source: S) -> JoinHandle<()> {
    let source = Arc::new(source);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(serve(stream, Arc::clone(&source)));
                }
                Err(err) => {
                    debug!("clipboard bridge accept failed: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    })
}

async fn serve<S: Source>(mut stream: UnixStream, source: Arc<S>) {
    let (reader, mut writer) = stream.split();
    let mut line = String::new();
    let read = tokio::time::timeout(REQUEST_TIMEOUT, BufReader::new(reader).read_line(&mut line)).await;
    let response = match read {
        Ok(Ok(_)) => answer(&line, source.as_ref()).await,
        Ok(Err(err)) => Response::Error(format!("unreadable request: {err}")),
        Err(_) => Response::Error("no request received".to_string()),
    };
    if let Err(err) = writer.write_all(&response.encode()).await {
        debug!("clipboard bridge reply failed: {err}");
    }
    let _ = writer.shutdown().await;
}

async fn answer<S: Source>(line: &str, source: &S) -> Response {
    match Request::parse(line) {
        None => Response::Error(format!("unknown request: {}", line.trim())),
        Some(Request::Targets) => {
            let listing: String = source.targets().await.iter().map(|t| format!("{}\n", t.mime())).collect();
            if listing.is_empty() { Response::None } else { Response::Ok(listing.into_bytes()) }
        }
        Some(Request::Get(target)) => match source.read(target).await {
            Ok(Some(bytes)) => Response::Ok(bytes),
            Ok(None) => Response::None,
            Err(err) => {
                warn!("clipboard read failed: {err:#}");
                Response::Error(format!("{err:#}"))
            }
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A clipboard holding exactly what the test put there.
    pub(crate) struct Fixed {
        pub image: Option<Vec<u8>>,
        pub text: Option<String>,
    }

    impl Source for Fixed {
        async fn targets(&self) -> Vec<Target> {
            let mut targets = Vec::new();
            if self.image.is_some() {
                targets.push(Target::Image);
            }
            if self.text.is_some() {
                targets.push(Target::Text);
            }
            targets
        }

        async fn read(&self, target: Target) -> Result<Option<Vec<u8>>> {
            Ok(match target {
                Target::Image => self.image.clone(),
                Target::Text => self.text.as_ref().map(|t| t.clone().into_bytes()),
            })
        }
    }

    #[tokio::test]
    async fn the_socket_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::super::host_socket(dir.path());
        let _listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::super::host_socket(dir.path());
        std::fs::write(&path, b"leftover").unwrap();
        assert!(bind(&path).is_ok());
    }

    #[tokio::test]
    async fn targets_list_what_the_clipboard_holds() {
        let both = Fixed { image: Some(vec![1]), text: Some("hi".into()) };
        assert_eq!(answer("targets\n", &both).await, Response::Ok(b"image/png\ntext/plain\n".to_vec()));

        let empty = Fixed { image: None, text: None };
        assert_eq!(answer("targets\n", &empty).await, Response::None);
    }

    #[tokio::test]
    async fn a_missing_kind_is_none_not_an_error() {
        let text_only = Fixed { image: None, text: Some("hi".into()) };
        assert_eq!(answer("get image/png\n", &text_only).await, Response::None);
        assert_eq!(answer("get text/plain\n", &text_only).await, Response::Ok(b"hi".to_vec()));
    }

    #[tokio::test]
    async fn garbage_gets_an_error_line() {
        let empty = Fixed { image: None, text: None };
        assert!(matches!(answer("PUT stuff\n", &empty).await, Response::Error(_)));
    }
}
