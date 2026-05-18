mod auth;
mod cli;
mod config;
mod crypto;
mod docker;
mod logging;
mod path;
mod remote;
mod sandbox;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{Cli, Command};
use sandbox::instance;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    logging::init(cli.debug);

    match cli.command {
        Command::Start(args) => instance::start(args)?,
        Command::Destroy(args) => instance::destroy(args)?,
        Command::Status => instance::status()?,
        Command::Login(args) => {
            auth::device_flow::login(args.api_url.as_deref()).await?;
            crypto::keys::ensure_identity()?;
            println!("  Identity keys ready.");
        }
        Command::Logout => {
            auth::token::clear_token()?;
            println!("Logged out.");
        }
        Command::Whoami => whoami()?,
        Command::Connect(args) => {
            let project_dir = std::fs::canonicalize(&args.path)
                .context("project directory does not exist")?;
            remote::connect::connect(
                &project_dir.to_string_lossy(),
                args.api_url.as_deref(),
            )
            .await?;
        }
        Command::Pair(args) => {
            match args.mode {
                cli::PairMode::Qr => remote::pair::pair_qr(args.api_url.as_deref()).await?,
                cli::PairMode::Password => remote::pair::pair_password(args.api_url.as_deref()).await?,
            }
        }
        Command::Chat(args) => {
            let project_dir = std::fs::canonicalize(&args.path)
                .context("project directory does not exist")?;
            let token = auth::token::require_valid_token()?;

            let base = cli::resolve_api_url(args.api_url.as_deref());
            let client = reqwest::Client::new();
            let resp: serde_json::Value = client
                .post(format!("{base}/api/sessions"))
                .bearer_auth(&token.access_token)
                .json(&serde_json::json!({
                    "projectName": project_dir.file_name()
                        .unwrap_or_default().to_string_lossy(),
                    "projectDir": project_dir.to_string_lossy(),
                    "instanceName": "chat",
                }))
                .send()
                .await
                .context("failed to create session")?
                .error_for_status()
                .context("API returned error")?
                .json()
                .await?;

            let relay_url = resp["relayUrl"].as_str()
                .context("missing relayUrl in response")?.to_string();
            let relay_token = resp["relayToken"].as_str()
                .context("missing relayToken in response")?.to_string();

            remote::chat::bridge_chat(
                &project_dir.to_string_lossy(),
                &args.prompt,
                relay_url,
                relay_token,
            )
            .await?;
        }
    }

    Ok(())
}

fn whoami() -> Result<()> {
    match auth::token::load_token()? {
        Some(token) => {
            if token.is_expired() {
                println!("  Session expired. Run `sandseal login` to re-authenticate.");
            } else {
                println!("  Logged in.");
                let t = &token.access_token;
                if t.len() > 12 {
                    println!("  Token: {}...{}", &t[..8], &t[t.len() - 4..]);
                }
                if let Some(exp) = token.expires_at {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    let remaining = exp.saturating_sub(now);
                    let hours = remaining / 3600;
                    let mins = (remaining % 3600) / 60;
                    println!("  Expires in: {}h {}m", hours, mins);
                }
                if crypto::keys::identity_exists() {
                    let kp = crypto::keys::ensure_identity()?;
                    println!("  Identity: {}", kp.public_key_base64());
                }
            }
        }
        None => {
            println!("  Not logged in. Run `sandseal login`.");
        }
    }
    Ok(())
}
