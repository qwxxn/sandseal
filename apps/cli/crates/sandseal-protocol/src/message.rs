use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageType {
    TerminalData = 0x01,
    TerminalResize = 0x02,
    ChatMessage = 0x03,
    ChatResponseChunk = 0x04,
    SessionMetadata = 0x05,
    Keepalive = 0x06,
    KeyRotation = 0x10,
    SessionEnd = 0xFF,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalResize {
    pub cols: u16,
    pub rows: u16,
}
