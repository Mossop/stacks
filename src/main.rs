mod commands;
mod config;
mod exec;
mod infer;
mod program;

use std::{env::current_dir, fs::File, path::PathBuf};

use clap::Parser;
use flexi_logger::{LevelFilter, LogSpecBuilder, Logger};

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

    Logger::with(
        LogSpecBuilder::new()
            .default(LevelFilter::Error)
            .module("stack", args.verbose.log_level_filter())
            .build(),
    )
    .start()
    .unwrap();

    let stacks_file = stacks_file(&args.file)?;
    log::debug!("Loading stacks from {}", stacks_file.display());
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
