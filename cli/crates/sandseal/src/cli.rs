use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const DEFAULT_API_URL: &str = "https://sandseal.io";

pub fn resolve_api_url(flag: Option<&str>) -> &str {
    flag.unwrap_or(DEFAULT_API_URL)
}

#[derive(Parser)]
#[command(name = "sandseal", version, about = "Isolated Docker sandboxes for AI coding agents")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Enable debug mode (bash shell instead of agent CLI)
    #[arg(short, long, global = true)]
    pub debug: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start a sandbox for the given project directory
    Start(StartArgs),
    /// Destroy sandbox(es) for a project
    Destroy(DestroyArgs),
    /// Show running sandbox instances
    Status,
    /// Log in to Sandseal via browser
    Login(LoginArgs),
    /// Log out and remove stored credentials
    Logout,
    /// Show current login status
    Whoami,
    /// Connect to a remote session via relay
    Connect(ConnectArgs),
    /// Pair this device with a browser session
    Pair(PairArgs),
    /// Run Claude Code and bridge output to a remote session
    Chat(ChatArgs),
}

#[derive(Parser)]
pub struct StartArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Force rebuild the Docker image
    #[arg(short, long)]
    pub rebuild: bool,

    /// Expose sandbox as a remote session via relay
    #[arg(long)]
    pub remote: bool,

    /// API server URL (for --remote)
    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,

    /// Arguments passed through to the agent CLI (after --)
    #[arg(last = true)]
    pub agent_args: Vec<String>,
}

#[derive(Parser)]
pub struct DestroyArgs {
    /// Project directory (defaults to current directory). Use --all to destroy everything.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Destroy all sandboxes
    #[arg(long)]
    pub all: bool,
}

#[derive(Parser)]
pub struct LoginArgs {
    /// API server URL
    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}

#[derive(Parser)]
pub struct ConnectArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// API server URL
    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}

#[derive(Parser)]
pub struct PairArgs {
    /// Pairing mode
    #[command(subcommand)]
    pub mode: PairMode,

    /// API server URL
    #[arg(long, global = true, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}

#[derive(Subcommand)]
pub enum PairMode {
    /// Pair via QR code — display URL for browser to scan
    Qr,
    /// Pair via password — display password for manual entry
    Password,
}

#[derive(Parser)]
pub struct ChatArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Prompt to send to Claude Code
    #[arg(short, long)]
    pub prompt: String,

    /// API server URL
    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}
