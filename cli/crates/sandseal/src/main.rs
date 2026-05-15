mod cli;
mod config;
mod crypto;
mod docker;
mod logging;
mod network;
mod path;
mod sandbox;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};
use sandbox::instance;

fn main() -> Result<()> {
    let cli = Cli::parse();
    logging::init(cli.debug);

    match cli.command {
        Command::Start(args) => instance::start(args)?,
        Command::Destroy(args) => instance::destroy(args)?,
        Command::Status => instance::status()?,
    }

    Ok(())
}
