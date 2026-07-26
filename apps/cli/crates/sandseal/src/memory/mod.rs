pub mod client;
pub mod mcp;
pub mod recall;

use anyhow::Result;

use crate::cli::{MemoryArgs, MemoryCommand};

pub async fn run(args: MemoryArgs) -> Result<()> {
    match args.command {
        MemoryCommand::Recall(recall_args) => {
            recall::run(recall_args.api_url.as_deref(), recall_args.stdin).await
        }
        MemoryCommand::Mcp(mcp_args) => mcp::serve(mcp_args.api_url.as_deref()).await,
    }
}
