//! Clipboard bridge: the host clipboard, served to the sandbox over a unix socket.
//!
//! Claude Code reads a pasted image by shelling out to `xclip` (then `wl-paste`, then on WSL
//! `powershell.exe`). None of those can work inside the container — there is no X11 or
//! Wayland socket to talk to, and Windows interop does not cross the container boundary — so
//! Ctrl+V with an image on the clipboard silently does nothing.
//!
//! The bridge closes that gap without exposing the display or the Windows drive: the CLI on
//! the host listens on a socket in the instance tmp dir, reads the clipboard with whatever the
//! host platform offers, and the sandbox gets an `xclip` stand-in that forwards the requests
//! Claude Code makes. Only the bytes of the clipboard cross into the container, and only
//! when asked for.
pub mod client;
pub mod host;
pub mod server;

use std::path::{Path, PathBuf};

/// Where the sandbox finds the socket; set by the compose override, read by the client.
pub const SOCKET_ENV: &str = "SANDSEAL_CLIPBOARD_SOCKET";
/// The socket's path inside the container.
pub const CONTAINER_SOCKET: &str = "/run/sandseal/clipboard.sock";
/// Where the `xclip` stand-in is mounted — ahead of `/usr/bin` on PATH.
pub const SHIM_MOUNT: &str = "/usr/local/bin/xclip";
/// The stand-in's location in the assets directory.
pub const SHIM_ASSET: &str = "agents/clipboard/xclip";

/// Host-side socket path for an instance, so the server and the compose mount cannot disagree.
pub fn host_socket(tmp_dir: &Path) -> PathBuf {
    tmp_dir.join("clipboard.sock")
}

/// What the sandbox can ask for. Named by MIME type on the wire, which is also what `xclip -t`
/// takes, so the client passes the agent's request through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Image,
    Text,
}

impl Target {
    pub fn mime(self) -> &'static str {
        match self {
            Target::Image => "image/png",
            Target::Text => "text/plain",
        }
    }

    pub fn parse(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Target::Image),
            "text/plain" => Some(Target::Text),
            _ => None,
        }
    }
}

/// One request per connection, one line each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Which targets the clipboard currently offers.
    Targets,
    /// The clipboard contents as one target.
    Get(Target),
}

impl Request {
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line == "targets" {
            return Some(Request::Targets);
        }
        line.strip_prefix("get ").and_then(Target::parse).map(Request::Get)
    }

    pub fn encode(self) -> String {
        match self {
            Request::Targets => "targets\n".to_string(),
            Request::Get(target) => format!("get {}\n", target.mime()),
        }
    }
}

/// A header line followed by the body: `ok <len>\n<bytes>`, `none\n`, `error <message>\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok(Vec<u8>),
    /// The clipboard holds nothing of the requested kind. Distinct from an error so the
    /// client can exit quietly the way `xclip` does — the agent probes for images on every
    /// paste, and most pastes are text.
    None,
    Error(String),
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Response::Ok(body) => {
                let mut out = format!("ok {}\n", body.len()).into_bytes();
                out.extend_from_slice(body);
                out
            }
            Response::None => b"none\n".to_vec(),
            Response::Error(message) => format!("error {}\n", message.replace('\n', " ")).into_bytes(),
        }
    }

    /// Decodes the header line; the body length is what the caller then reads.
    pub fn parse_header(line: &str) -> Option<Header> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "none" {
            return Some(Header::None);
        }
        if let Some(len) = line.strip_prefix("ok ") {
            return len.parse().ok().map(Header::Ok);
        }
        line.strip_prefix("error ").map(|m| Header::Error(m.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    Ok(usize),
    None,
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for request in [Request::Targets, Request::Get(Target::Image), Request::Get(Target::Text)] {
            assert_eq!(Request::parse(&request.encode()), Some(request));
        }
    }

    #[test]
    fn unknown_targets_are_rejected_not_guessed() {
        // Claude Code also asks for image/bmp; answering with a PNG would corrupt the paste.
        assert_eq!(Request::parse("get image/bmp"), None);
        assert_eq!(Request::parse("get"), None);
        assert_eq!(Request::parse(""), None);
    }

    #[test]
    fn responses_carry_their_length() {
        let encoded = Response::Ok(b"\x89PNG".to_vec()).encode();
        assert_eq!(&encoded[..5], b"ok 4\n");
        assert_eq!(&encoded[5..], b"\x89PNG");
        assert_eq!(Response::parse_header("ok 4\n"), Some(Header::Ok(4)));
        assert_eq!(Response::parse_header("none\n"), Some(Header::None));
        assert_eq!(Response::parse_header("error boom\n"), Some(Header::Error("boom".into())));
        assert_eq!(Response::parse_header("ok many\n"), None);
    }

    #[test]
    fn error_messages_stay_on_one_line() {
        let encoded = Response::Error("first\nsecond".into()).encode();
        assert_eq!(encoded, b"error first second\n");
    }
}
