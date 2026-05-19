use std::io::Write;

use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

const DISPLAY_DURATION: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
pub enum OverlayLevel {
    Info,
    Warn,
}

pub struct OverlayMessage {
    pub text: String,
    pub level: OverlayLevel,
}

impl OverlayMessage {
    pub fn info(text: impl Into<String>) -> Self {
        Self { text: text.into(), level: OverlayLevel::Info }
    }

    pub fn warn(text: impl Into<String>) -> Self {
        Self { text: text.into(), level: OverlayLevel::Warn }
    }
}

pub async fn run_overlay(mut rx: mpsc::UnboundedReceiver<OverlayMessage>, cols: u16) {
    while let Some(msg) = rx.recv().await {
        let rendered_len = render(&msg, cols);
        let clear_cols = cols;
        tokio::spawn(async move {
            sleep(DISPLAY_DURATION).await;
            clear(rendered_len, clear_cols);
        });
    }
}

fn render(msg: &OverlayMessage, cols: u16) -> u16 {
    let (label, color) = match msg.level {
        OverlayLevel::Info => ("▸", "\x1b[90m"),
        OverlayLevel::Warn => ("▸", "\x1b[33m"),
    };

    let content = format!(" {} {} ", label, msg.text);
    let width = content.len() as u16;
    let col = cols.saturating_sub(width) + 1;

    let seq = format!("\x1b7\x1b[1;{col}H{color}{content}\x1b[0m\x1b8");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();

    width
}

fn clear(width: u16, cols: u16) {
    let col = cols.saturating_sub(width) + 1;
    let spaces = " ".repeat(width as usize);
    let seq = format!("\x1b7\x1b[1;{col}H{spaces}\x1b8");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}
