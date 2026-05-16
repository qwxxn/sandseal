use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sandseal", about = "Isolated Docker sandboxes for AI coding agents")]
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
}

#[derive(Parser)]
pub struct StartArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Force rebuild the Docker image
    #[arg(short, long)]
    pub rebuild: bool,

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
    /// API server URL (defaults to https://sandseal.io)
    #[arg(long)]
    pub api_url: Option<String>,
}

#[derive(Parser)]
pub struct ConnectArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// API server URL (defaults to https://sandseal.io)
    #[arg(long)]
    pub api_url: Option<String>,
}
