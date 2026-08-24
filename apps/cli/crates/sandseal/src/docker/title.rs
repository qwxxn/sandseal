//! Terminal-title handling for an attached sandbox.
//!
//! The agent inside the container names the window with an OSC sequence
//! (`ESC ] 0 ; text BEL`). Those bytes reach the host terminal untouched, but they say
//! nothing about which sandbox produced them — with several sessions open every window
//! ends up called the same thing. This module rewrites the payload as it streams past,
//! putting the sandbox identity in front of whatever the agent chose.
//!
//! Two things only the CLI can do, because they depend on the host and the agent cannot
//! see the host:
//!
//! - **Seed the title.** The agent sets one only when it has something to say, so a fresh
//!   window keeps whatever name it had until then. The CLI names it at attach time.
//! - **Escape a multiplexer.** `TMUX` / `STY` exist in the CLI's environment and never in
//!   the container's, so the agent cannot know it is inside tmux and always emits a bare
//!   OSC, which the multiplexer may swallow. The CLI wraps the same sequence in DCS
//!   passthrough as well. A tmux that does not allow passthrough discards the wrapper
//!   silently rather than printing it, so sending both is safe either way.

use std::io::Write;

/// Longest OSC payload we will hold while looking for a terminator. A title is short; a
/// sequence this long is either not a title or a stream that will never terminate one, and
/// either way it must not be buffered indefinitely.
const MAX_OSC: usize = 1024;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Renders a window title in every form the host terminal might listen to.
pub struct TitleWriter {
    /// Sandbox identity, prepended to whatever the agent sets.
    prefix: String,
    /// Host multiplexer detected from the CLI's own environment.
    mux: Option<Mux>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mux {
    Tmux,
    Screen,
}

impl TitleWriter {
    pub fn new(prefix: String) -> Self {
        Self { prefix, mux: detect_mux() }
    }

    /// The full byte sequence that sets the window title to `payload`, prefixed.
    pub fn render(&self, payload: &str) -> Vec<u8> {
        let text = compose(&self.prefix, payload);
        let osc = format!("\x1b]0;{text}\x07");

        let mut out = osc.clone().into_bytes();
        if let Some(mux) = self.mux {
            out.extend_from_slice(passthrough(mux, &osc).as_bytes());
        }
        out
    }
}

/// `prefix ▸ payload`, or the prefix alone when the agent has not named anything yet.
fn compose(prefix: &str, payload: &str) -> String {
    let payload = payload.trim();
    if payload.is_empty() {
        prefix.to_string()
    } else if prefix.is_empty() {
        payload.to_string()
    } else {
        format!("{prefix} \u{25b8} {payload}")
    }
}

/// Wrap a sequence so a multiplexer forwards it to the terminal it is itself drawing on.
/// Both forms require every inner ESC to be doubled.
fn passthrough(mux: Mux, seq: &str) -> String {
    let escaped = seq.replace('\x1b', "\x1b\x1b");
    match mux {
        Mux::Tmux => format!("\x1bPtmux;{escaped}\x1b\\"),
        Mux::Screen => format!("\x1bP{escaped}\x1b\\"),
    }
}

fn detect_mux() -> Option<Mux> {
    if std::env::var_os("TMUX").is_some() {
        Some(Mux::Tmux)
    } else if std::env::var_os("STY").is_some() {
        Some(Mux::Screen)
    } else {
        None
    }
}

#[derive(Debug, PartialEq)]
enum State {
    /// Copying bytes through untouched.
    Passthrough,
    /// Saw ESC, waiting to find out whether an OSC follows.
    Esc,
    /// Inside `ESC ]`, collecting the payload up to its terminator.
    Osc,
    /// Inside an OSC and saw ESC, which may begin the `ESC \` terminator.
    OscEsc,
}

/// Streaming rewriter for window-title sequences in the attached output.
///
/// Everything that is not a complete OSC 0/1/2 sequence is forwarded byte for byte,
/// including sequences split across reads and anything that overruns [`MAX_OSC`]. The
/// filter never withholds output it has decided to pass on, so a title it fails to
/// recognise costs nothing beyond an unprefixed window name.
pub struct TitleFilter {
    writer: TitleWriter,
    state: State,
    /// Bytes of the sequence being examined, held back until it resolves.
    pending: Vec<u8>,
}

impl TitleFilter {
    pub fn new(writer: TitleWriter) -> Self {
        Self { writer, state: State::Passthrough, pending: Vec::new() }
    }

    /// Feed one read of the attached stream; returns what should reach the terminal.
    pub fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());

        for &byte in input {
            match self.state {
                State::Passthrough => {
                    if byte == ESC {
                        self.state = State::Esc;
                        self.pending.push(byte);
                    } else {
                        out.push(byte);
                    }
                }
                State::Esc => {
                    self.pending.push(byte);
                    if byte == b']' {
                        self.state = State::Osc;
                    } else {
                        // Some other escape sequence — none of our business.
                        out.append(&mut self.pending);
                        self.state = State::Passthrough;
                    }
                }
                State::Osc => {
                    if byte == BEL {
                        self.pending.push(byte);
                        self.emit(&mut out);
                    } else if byte == ESC {
                        self.pending.push(byte);
                        self.state = State::OscEsc;
                    } else {
                        self.pending.push(byte);
                        if self.pending.len() > MAX_OSC {
                            out.append(&mut self.pending);
                            self.state = State::Passthrough;
                        }
                    }
                }
                State::OscEsc => {
                    self.pending.push(byte);
                    if byte == b'\\' {
                        self.emit(&mut out);
                    } else {
                        // An ESC inside an OSC that did not terminate it. Give up on this
                        // one rather than guess; the bytes go out exactly as they came in.
                        out.append(&mut self.pending);
                        self.state = State::Passthrough;
                    }
                }
            }
        }

        out
    }

    /// Release any half-read sequence verbatim. Call once the stream ends, so a sandbox
    /// that dies mid-sequence does not swallow the bytes it had already written.
    pub fn flush(&mut self) -> Vec<u8> {
        self.state = State::Passthrough;
        std::mem::take(&mut self.pending)
    }

    /// Resolve a complete OSC: rewrite it if it names the window, forward it if not.
    fn emit(&mut self, out: &mut Vec<u8>) {
        let seq = std::mem::take(&mut self.pending);
        self.state = State::Passthrough;

        match title_payload(&seq) {
            Some(payload) => out.extend_from_slice(&self.writer.render(payload)),
            None => out.extend_from_slice(&seq),
        }
    }
}

/// The text of an OSC that sets the window title, or `None` for any other OSC.
///
/// OSC 0 sets title and icon, OSC 2 sets the title; OSC 1 is the icon name alone and is
/// left alone with everything else.
fn title_payload(seq: &[u8]) -> Option<&str> {
    let body = seq.strip_prefix(&[ESC, b']'])?;
    let body = body
        .strip_suffix(&[BEL])
        .or_else(|| body.strip_suffix(&[ESC, b'\\']))?;

    let split = body.iter().position(|&b| b == b';')?;
    let (ps, rest) = body.split_at(split);

    match ps {
        b"0" | b"2" => std::str::from_utf8(&rest[1..]).ok(),
        _ => None,
    }
}

/// Write the opening title, before the agent has drawn anything.
pub fn seed(writer: &TitleWriter) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&writer.render(""));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(prefix: &str) -> TitleFilter {
        TitleFilter::new(TitleWriter { prefix: prefix.to_string(), mux: None })
    }

    #[test]
    fn plain_output_passes_through_unchanged() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"hello world\n"), b"hello world\n");
    }

    #[test]
    fn title_gets_the_prefix() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]0;topic\x07"), b"\x1b]0;box \xe2\x96\xb8 topic\x07");
    }

    #[test]
    fn empty_title_falls_back_to_the_prefix() {
        // The agent clears the title on exit; that must not leave the window nameless.
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]0;\x07"), b"\x1b]0;box\x07");
    }

    #[test]
    fn osc_2_is_a_title_too() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]2;topic\x07"), b"\x1b]0;box \xe2\x96\xb8 topic\x07");
    }

    #[test]
    fn st_terminator_is_accepted() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]0;topic\x1b\\"), b"\x1b]0;box \xe2\x96\xb8 topic\x07");
    }

    #[test]
    fn other_osc_codes_are_left_alone() {
        // OSC 8 is a hyperlink, OSC 52 the clipboard — rewriting either would break them.
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]8;;http://x\x07"), b"\x1b]8;;http://x\x07");
        assert_eq!(f.feed(b"\x1b]52;c;Zm9v\x07"), b"\x1b]52;c;Zm9v\x07");
        assert_eq!(f.feed(b"\x1b]1;icon\x07"), b"\x1b]1;icon\x07");
    }

    #[test]
    fn ordinary_escape_sequences_are_left_alone() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b[31mred\x1b[0m"), b"\x1b[31mred\x1b[0m");
        assert_eq!(f.feed(b"\x1b[?25l"), b"\x1b[?25l");
    }

    #[test]
    fn sequence_split_across_reads_is_reassembled() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"a\x1b]0;to"), b"a");
        assert_eq!(f.feed(b"pic\x07b"), b"\x1b]0;box \xe2\x96\xb8 topic\x07b");
    }

    #[test]
    fn escape_split_across_reads_is_not_eaten() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b"), b"");
        assert_eq!(f.feed(b"[0m"), b"\x1b[0m");
    }

    #[test]
    fn an_overlong_osc_is_released_rather_than_buffered() {
        let mut f = filter("box");
        let long = [b'x'; MAX_OSC + 16];
        let mut input = b"\x1b]0;".to_vec();
        input.extend_from_slice(&long);

        let out = f.feed(&input);
        assert_eq!(out, input, "bytes must survive even when we stop tracking them");
        assert_eq!(f.state, State::Passthrough);
    }

    #[test]
    fn flush_releases_a_half_read_sequence() {
        let mut f = filter("box");
        assert_eq!(f.feed(b"\x1b]0;half"), b"");
        assert_eq!(f.flush(), b"\x1b]0;half");
        assert_eq!(f.flush(), b"", "flushing twice must not repeat the bytes");
    }

    #[test]
    fn utf8_payload_survives() {
        let mut f = filter("box");
        let out = f.feed("\x1b]0;✳ oprava\x07".as_bytes());
        assert_eq!(out, "\x1b]0;box ▸ ✳ oprava\x07".as_bytes());
    }

    #[test]
    fn an_empty_prefix_leaves_the_agent_title_alone() {
        let mut f = filter("");
        assert_eq!(f.feed(b"\x1b]0;topic\x07"), b"\x1b]0;topic\x07");
    }

    #[test]
    fn tmux_gets_the_passthrough_copy_as_well() {
        let w = TitleWriter { prefix: "box".into(), mux: Some(Mux::Tmux) };
        let out = String::from_utf8(w.render("topic")).unwrap();
        assert_eq!(out, "\x1b]0;box ▸ topic\x07\x1bPtmux;\x1b\x1b]0;box ▸ topic\x07\x1b\\");
    }

    #[test]
    fn screen_gets_its_own_wrapper() {
        let w = TitleWriter { prefix: "box".into(), mux: Some(Mux::Screen) };
        let out = String::from_utf8(w.render("topic")).unwrap();
        assert_eq!(out, "\x1b]0;box ▸ topic\x07\x1bP\x1b\x1b]0;box ▸ topic\x07\x1b\\");
    }
}
