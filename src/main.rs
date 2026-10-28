mod commands;
mod config;
mod exec;
mod infer;
mod portainer;
mod program;

use std::{env::current_dir, fs::File, path::PathBuf};

use clap::Parser;
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    fmt,
    prelude::*,
};

use program::Program;

use config::Config;

fn stacks_file(file: &Option<String>) -> Result<PathBuf, String> {
    let mut dir = current_dir().map_err(|e| format!("Current directory is invalid: {}", e))?;

    match file {
        Some(path) => {
            let mut target = dir;
            target.push(path);
            target = target
                .canonicalize()
                .map_err(|e| format!("Invalid path: {}", e))?;

            if target.is_file() {
                Ok(target)
            } else {
                Err(format!(
                    "The file {} does not exist or is not a file.",
                    path
                ))
            }
        }
        None => {
            loop {
                let mut target = dir.clone();
                target.push("stacks.yml");
                if target.is_file() {
                    return target
                        .canonicalize()
                        .map_err(|e| format!("Invalid path: {}", e));
                }

                dir = match dir.parent() {
                    Some(path) => path.to_path_buf(),
                    None => break,
                }
            }

            Err(
                "No stacks.yml file present in the current directory or any of its parents."
                    .to_string(),
            )
        }
    }
}

fn run() -> Result<(), String> {
    let args = Program::parse();

    tracing_subscriber::registry()
        .with(fmt::layer().without_time().with_writer(std::io::stderr))
        .with(
            Targets::new()
                .with_default(LevelFilter::ERROR)
                .with_target("stacks", args.verbose.tracing_level_filter()),
        )
        .init();

    let stacks_file = stacks_file(&args.file)?;
    tracing::debug!("Loading stacks from {}", stacks_file.display());
    let f = File::open(&stacks_file)
        .map_err(|e| format!("Failed to open file {}: {}", stacks_file.display(), e))?;

    let mut config = Config::from_reader(stacks_file.parent().unwrap(), f)?;
    if args.command.uses_dependencies() {
        config.add_inferred_dependencies()?;
    }

    args.command.run(&args.globals, &config)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", e);
    }
}
