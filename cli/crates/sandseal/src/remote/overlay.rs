use std::io::Write;

use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

const DISPLAY_DURATION: Duration = Duration::from_secs(4);
const DEFAULT_TITLE: &str = "sandseal";

pub struct OverlayMessage {
    pub text: String,
}

impl OverlayMessage {
    pub fn info(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

pub async fn run_overlay(mut rx: mpsc::UnboundedReceiver<OverlayMessage>) {
    while let Some(msg) = rx.recv().await {
        set_title(&format!("sandseal ▸ {}", msg.text));
        tokio::spawn(async {
            sleep(DISPLAY_DURATION).await;
            set_title(DEFAULT_TITLE);
        });
    }
}

fn set_title(title: &str) {
    let seq = format!("\x1b]2;{title}\x07");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}
