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
    /// Build (or rebuild) the sandbox image without starting a container
    Build(BuildArgs),
    /// Destroy sandbox(es) for a project
    Destroy(DestroyArgs),
    /// Show running sandbox instances
    Status,
    /// Manage named configuration profiles
    Config(ConfigArgs),
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
    /// Memory bridge used inside the sandbox (MCP server and recall hook)
    Memory(MemoryArgs),
    /// Update Sandseal to the latest release
    Update(UpdateArgs),
}

#[derive(Parser)]
pub struct UpdateArgs {
    /// Install a specific version instead of the latest (also downgrades)
    #[arg(long = "version", value_name = "X.Y.Z")]
    pub version: Option<String>,
}

#[derive(Parser)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// Retrieve relevant notes for a prompt (UserPromptSubmit hook entry point)
    Recall(MemoryRecallArgs),
    /// Serve the memory tools to the agent as a local MCP server over stdio
    Mcp(MemoryMcpArgs),
}

#[derive(Parser)]
pub struct MemoryRecallArgs {
    /// Read the hook payload as JSON from stdin
    #[arg(long)]
    pub stdin: bool,

    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}

#[derive(Parser)]
pub struct MemoryMcpArgs {
    #[arg(long, env = "SANDSEAL_API_URL")]
    pub api_url: Option<String>,
}

#[derive(Parser)]
pub struct StartArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Force rebuild the Docker image
    #[arg(short, long)]
    pub rebuild: bool,

    /// Configuration profile to apply (overrides the active one)
    #[arg(long)]
    pub profile: Option<String>,

    /// Ignore the active configuration profile for this run
    #[arg(long, conflicts_with = "profile")]
    pub no_profile: bool,

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
pub struct BuildArgs {
    /// Project directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Configuration profile to apply (overrides the active one)
    #[arg(long)]
    pub profile: Option<String>,

    /// Ignore the active configuration profile for this build
    #[arg(long, conflicts_with = "profile")]
    pub no_profile: bool,
}

#[derive(Parser)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,

    /// Project directory the active profile applies to
    #[arg(long, global = true, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available profiles
    List,
    /// Set the active profile
    Set(ConfigSetArgs),
    /// Clear the active profile
    Unset(ConfigScopeArgs),
    /// Print a profile's contents
    Show(ConfigNameArgs),
    /// Create a new profile
    Create(ConfigCreateArgs),
    /// Delete a profile
    Delete(ConfigDeleteArgs),
    /// Open a profile in $EDITOR
    Edit(ConfigNameArgs),
    /// Print the effective settings after merging all layers
    Effective(ConfigEffectiveArgs),
}

#[derive(Parser)]
pub struct ConfigSetArgs {
    /// Profile name
    pub name: String,

    /// Set for every project on this machine instead of just this one
    #[arg(short, long)]
    pub global: bool,
}

#[derive(Parser)]
pub struct ConfigScopeArgs {
    /// Clear the machine-wide setting instead of this project's
    #[arg(short, long)]
    pub global: bool,
}

#[derive(Parser)]
pub struct ConfigNameArgs {
    /// Profile name (defaults to the active profile)
    pub name: Option<String>,
}

#[derive(Parser)]
pub struct ConfigCreateArgs {
    /// Profile name
    pub name: String,

    /// Copy an existing profile as the starting point
    #[arg(long)]
    pub from: Option<String>,

    /// Make the new profile active for this project
    #[arg(long)]
    pub activate: bool,
}

#[derive(Parser)]
pub struct ConfigDeleteArgs {
    /// Profile name
    pub name: String,
}

#[derive(Parser)]
pub struct ConfigEffectiveArgs {
    /// Use this profile instead of the active one
    #[arg(long)]
    pub profile: Option<String>,

    /// Ignore the active profile
    #[arg(long, conflicts_with = "profile")]
    pub no_profile: bool,
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
