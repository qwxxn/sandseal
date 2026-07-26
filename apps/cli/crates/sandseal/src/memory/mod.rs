pub mod client;
pub mod mcp;
pub mod provision;
pub mod recall;
pub mod session;

use anyhow::Result;

use crate::cli::{MemoryArgs, MemoryCommand};

pub async fn run(args: MemoryArgs) -> Result<()> {
    match args.command {
        MemoryCommand::Recall(a) => recall::run(a.api_url.as_deref(), a.stdin).await,
        MemoryCommand::Mcp(a) => mcp::serve(a.api_url.as_deref()).await,
    }
}
