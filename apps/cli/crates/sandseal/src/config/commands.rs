use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::cli::{
    ConfigArgs, ConfigCommand, ConfigCreateArgs, ConfigDeleteArgs, ConfigEffectiveArgs,
    ConfigNameArgs, ConfigScopeArgs, ConfigSetArgs,
};
use crate::config::load::{self, ProfileChoice};
use crate::config::state::{self, Scope};
use crate::config::{profile, validate::validate_settings};

pub fn run(args: ConfigArgs) -> Result<()> {
    let project_dir = resolve_project_dir(&args.path)?;

    match args.command {
        ConfigCommand::List => list(&project_dir),
        ConfigCommand::Set(a) => set(&project_dir, a),
        ConfigCommand::Unset(a) => unset(&project_dir, a),
        ConfigCommand::Show(a) => show(&project_dir, a),
        ConfigCommand::Create(a) => create(&project_dir, a),
        ConfigCommand::Delete(a) => delete(a),
        ConfigCommand::Edit(a) => edit(&project_dir, a),
        ConfigCommand::Effective(a) => effective(&project_dir, a),
    }
}

fn resolve_project_dir(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| format!("project directory does not exist: {}", path.display()))
}

/// The profile a bare `config show`/`edit` refers to.
fn require_active(project_dir: &Path, name: Option<String>) -> Result<String> {
    if let Some(name) = name {
        return Ok(name);
    }
    match state::active_profile(project_dir)? {
        Some((name, _)) => Ok(name),
        None => bail!("no active profile — pass a name or run `sandseal config set <name>`"),
    }
}

fn list(project_dir: &Path) -> Result<()> {
    let profiles = profile::list()?;
    let active = state::active_profile(project_dir)?;

    println!("  Profiles ({}):", profile::profiles_dir()?.display());
    println!();

    if profiles.is_empty() {
        println!("    (none) — create one with `sandseal config create <name>`");
    } else {
        let active_name = active.as_ref().map(|(n, _)| n.as_str());
        let width = profiles.iter().map(|p| p.name.len()).max().unwrap_or(0);

        for p in &profiles {
            let marker = if Some(p.name.as_str()) == active_name { "*" } else { " " };
            let line = match &p.description {
                Some(d) => format!("  {marker} {:width$}  {d}", p.name),
                None => format!("  {marker} {}", p.name),
            };
            println!("{}", line.trim_end());
        }
    }

    println!();
    match &active {
        Some((name, scope)) => {
            println!("  Active: {name} ({scope})");
            if !profile::exists(name) {
                println!("  WARNING: profile '{name}' is missing — `sandseal start` will fail.");
            }
        }
        None => println!("  Active: none"),
    }

    Ok(())
}

fn set(project_dir: &Path, args: ConfigSetArgs) -> Result<()> {
    if !profile::exists(&args.name) {
        profile::validate_name(&args.name)?;
        bail!(
            "profile '{}' not found. Create it with `sandseal config create {}`.",
            args.name,
            args.name
        );
    }

    let scope = if args.global { Scope::Global } else { Scope::Project };
    let path = state::set_active_profile(project_dir, scope, Some(&args.name))?;

    println!("  Active profile: {} ({scope})", args.name);
    println!("  Wrote {}", path.display());

    if scope == Scope::Project {
        println!("  Add .sandseal/state.json to .gitignore — it is machine-local.");
    }

    Ok(())
}

fn unset(project_dir: &Path, args: ConfigScopeArgs) -> Result<()> {
    let scope = if args.global { Scope::Global } else { Scope::Project };
    state::set_active_profile(project_dir, scope, None)?;

    match state::active_profile(project_dir)? {
        Some((name, remaining)) => println!("  Cleared {scope} profile. Now active: {name} ({remaining})"),
        None => println!("  Cleared {scope} profile. No profile active."),
    }

    Ok(())
}

fn show(project_dir: &Path, args: ConfigNameArgs) -> Result<()> {
    let name = require_active(project_dir, args.name)?;
    let path = profile::profile_path(&name)?;

    if !path.is_file() {
        bail!("profile '{name}' not found");
    }

    println!("  {}", path.display());
    println!();
    print!("{}", std::fs::read_to_string(&path)?);

    Ok(())
}

fn create(project_dir: &Path, args: ConfigCreateArgs) -> Result<()> {
    let path = profile::create(&args.name, args.from.as_deref())?;
    println!("  Created profile '{}': {}", args.name, path.display());

    if args.activate {
        state::set_active_profile(project_dir, Scope::Project, Some(&args.name))?;
        println!("  Active profile: {} (project)", args.name);
    } else {
        println!("  Activate it with `sandseal config set {}`.", args.name);
    }

    Ok(())
}

fn delete(args: ConfigDeleteArgs) -> Result<()> {
    let path = profile::delete(&args.name)?;
    println!("  Deleted {}", path.display());
    println!("  Projects still pointing at it will fail until you run `sandseal config unset`.");
    Ok(())
}

fn edit(project_dir: &Path, args: ConfigNameArgs) -> Result<()> {
    let name = require_active(project_dir, args.name)?;
    let path = profile::profile_path(&name)?;

    if !path.is_file() {
        bail!("profile '{name}' not found. Create it with `sandseal config create {name}`.");
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;

    if !status.success() {
        bail!("editor '{editor}' exited with {status}");
    }

    // Report a broken edit now rather than at the next `sandseal start`.
    validate_settings(&path)?;
    println!("  Profile '{name}' is valid.");

    Ok(())
}

fn effective(project_dir: &Path, args: ConfigEffectiveArgs) -> Result<()> {
    let choice = ProfileChoice::from_flags(args.profile.as_deref(), args.no_profile);
    let resolved = load::resolve(project_dir, &choice)?;

    match &resolved.profile {
        Some((name, source)) => println!("  Profile: {name} (from {source})"),
        None => println!("  Profile: none"),
    }
    println!("  Project: {}", project_dir.display());
    println!();
    println!("{}", serde_json::to_string_pretty(&resolved.value)?);

    Ok(())
}
