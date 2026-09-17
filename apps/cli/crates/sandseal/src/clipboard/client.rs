//! The sandbox side: an `xclip` stand-in, answering the calls Claude Code makes.
//!
//! Claude Code asks two things of xclip when pasting — `-t TARGETS -o` to learn whether an
//! image is on the clipboard, and `-t image/png -o` to fetch it — plus `-t text/plain -o`
//! for the path of a copied file. That is the whole surface: the stand-in speaks xclip's
//! conventions (bytes on stdout, exit 1 when there is nothing to give) and forwards each
//! request to the bridge on the host.
use std::io::Write;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{Header, Request, Response, SOCKET_ENV, Target};

/// One parsed xclip invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Request(Request),
    /// Anything the bridge does not serve: a target it has no answer for, or input mode
    /// (`xclip -i`, the default) — copying into the host clipboard is not bridged.
    Unsupported(String),
}

/// Runs one invocation; the return value is the process exit code.
pub async fn run(args: &[String]) -> Result<i32> {
    let request = match parse_args(args) {
        Invocation::Request(request) => request,
        Invocation::Unsupported(why) => {
            eprintln!("xclip (sandseal clipboard bridge): {why}");
            return Ok(1);
        }
    };

    let Some(socket) = std::env::var_os(SOCKET_ENV) else {
        eprintln!("xclip (sandseal clipboard bridge): {SOCKET_ENV} is not set, no host clipboard here");
        return Ok(1);
    };

    let mut stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("cannot reach the clipboard bridge at {}", socket.to_string_lossy()))?;
    stream.write_all(request.encode().as_bytes()).await?;

    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    reader.read_line(&mut header).await.context("clipboard bridge closed without answering")?;

    match Response::parse_header(&header) {
        Some(Header::Ok(len)) => {
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await.context("clipboard bridge sent a short reply")?;
            let mut out = std::io::stdout().lock();
            out.write_all(&body)?;
            out.flush()?;
            Ok(0)
        }
        Some(Header::None) => Ok(1),
        Some(Header::Error(message)) => bail!("clipboard bridge: {message}"),
        None => bail!("clipboard bridge sent an unreadable reply: {}", header.trim()),
    }
}

/// xclip's own flags, as far as the bridge honours them. Unknown flags are skipped rather
/// than rejected, so `-selection clipboard` and friends pass through as they always did.
pub fn parse_args(args: &[String]) -> Invocation {
    let mut target: Option<String> = None;
    let mut output = false;
    let mut input = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        // xclip accepts any unambiguous prefix of a flag; these are the spellings in use.
        match arg.as_str() {
            "-o" | "-out" | "-output" => output = true,
            "-i" | "-in" | "-input" => input = true,
            "-t" | "-target" => target = iter.next().cloned(),
            "-selection" | "-sel" | "-display" | "-d" | "-l" | "-loops" => {
                iter.next();
            }
            _ => {}
        }
    }

    if input || !output {
        return Invocation::Unsupported(
            "only reading the host clipboard is bridged; copying into it is not".to_string(),
        );
    }

    match target.as_deref() {
        None | Some("UTF8_STRING") | Some("STRING") => Invocation::Request(Request::Get(Target::Text)),
        Some(t) if t.eq_ignore_ascii_case("TARGETS") => Invocation::Request(Request::Targets),
        Some(t) => match Target::parse(t) {
            Some(target) => Invocation::Request(Request::Get(target)),
            None => Invocation::Unsupported(format!("target {t} is not served by the bridge")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_calls_claude_code_makes_are_understood() {
        assert_eq!(
            parse_args(&args("-selection clipboard -t TARGETS -o")),
            Invocation::Request(Request::Targets)
        );
        assert_eq!(
            parse_args(&args("-selection clipboard -t image/png -o")),
            Invocation::Request(Request::Get(Target::Image))
        );
        assert_eq!(
            parse_args(&args("-selection clipboard -t text/plain -o")),
            Invocation::Request(Request::Get(Target::Text))
        );
    }

    #[test]
    fn a_bare_output_request_means_text() {
        assert_eq!(parse_args(&args("-selection clipboard -o")), Invocation::Request(Request::Get(Target::Text)));
    }

    #[test]
    fn unserved_targets_fail_instead_of_returning_the_wrong_bytes() {
        // Claude Code falls through to image/bmp after image/png; a PNG there would be a
        // corrupt paste, so it must get the same exit 1 a real xclip gives.
        assert!(matches!(
            parse_args(&args("-selection clipboard -t image/bmp -o")),
            Invocation::Unsupported(_)
        ));
    }

    #[test]
    fn input_mode_is_refused() {
        assert!(matches!(parse_args(&args("-selection clipboard")), Invocation::Unsupported(_)));
        assert!(matches!(parse_args(&args("-i -selection clipboard")), Invocation::Unsupported(_)));
    }

    #[tokio::test]
    async fn end_to_end_over_a_socket() {
        use super::super::server;

        let dir = tempfile::tempdir().unwrap();
        let socket = super::super::host_socket(dir.path());
        let listener = server::bind(&socket).unwrap();
        let png = b"\x89PNG\r\n\x1a\n....".to_vec();
        let _bridge = server::spawn(
            listener,
            server::tests::Fixed { image: Some(png.clone()), text: None },
        );

        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(Request::Get(Target::Image).encode().as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, Response::Ok(png).encode());

        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(Request::Get(Target::Text).encode().as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"none\n");
    }
}
