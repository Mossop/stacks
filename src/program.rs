use std::collections::HashSet;

use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::{commands::run_against_stacks, config::Config};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, subcommand_precedence_over_arg(true))]
pub struct Program {
    /// The location of the stacks config file. By default looks for stacks.yml
    /// in the current and parent directories.
    #[arg(short, long, env = "STACKS_FILE")]
    pub file: Option<String>,

    #[clap(flatten)]
    pub verbose: Verbosity<InfoLevel>,

    #[command(flatten)]
    pub globals: GlobalArguments,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Args, Debug)]
pub struct GlobalArguments {
    /// A comma separated list of stacks to apply the command to. If not present
    /// or `*` is given then all stacks are used.
    stacks: Option<String>,
}

impl GlobalArguments {
    pub fn stacks(&self) -> Vec<&str> {
        match self.stacks {
            Some(ref s) => {
                if s.is_empty() || s == "*" {
                    Vec::new()
                } else {
                    s.split(',').collect()
                }
            }
            None => Vec::new(),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Build or rebuild services
    Build {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Copy files/folders between a service container and the local filesystem
    Cp {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Creates containers for a service.
    Create {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Stop and remove containers, networks
    Down {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Receive real time events from containers.
    Events {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Execute a command in a running container.
    Exec {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List images used by the created containers
    Images {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Force stop service containers.
    Kill {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// View output from containers
    Logs {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pause services
    Pause {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the public port for a port binding.
    Port {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List containers
    Ps {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pull service images
    Pull {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Push service images
    Push {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Restart service containers
    Restart {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Removes stopped service containers
    Rm {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a one-off command on a service.
    Run {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start services
    Start {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Stop services
    Stop {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Display the running processes
    Top {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Unpause services
    Unpause {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Create and start containers detached
    Up {
        /// Arguments to pass through to docker compose
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn reverse<T>(stacks: Vec<T>) -> Vec<T> {
    stacks.into_iter().rev().collect()
}

impl Commands {
    /// Whether this command needs to know about dependencies between stacks.
    pub fn uses_dependencies(&self) -> bool {
        matches!(
            self,
            Commands::Create { .. }
                | Commands::Down { .. }
                | Commands::Kill { .. }
                | Commands::Pause { .. }
                | Commands::Restart { .. }
                | Commands::Rm { .. }
                | Commands::Unpause { .. }
                | Commands::Up { .. }
        )
    }

    pub fn run(&self, globals: &GlobalArguments, config: &Config) -> Result<(), String> {
        match self {
            Commands::Build { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("build", config, &stacks, args)
            }
            Commands::Cp { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("cp", config, &stacks, args)
            }
            Commands::Create { args } => {
                let stacks = config.stacks_with_dependencies(globals.stacks())?;
                run_against_stacks("create", config, &stacks, args)
            }
            Commands::Down { args } => {
                let stacks = reverse(config.stacks_with_dependants(globals.stacks())?);
                run_against_stacks("down", config, &stacks, args)
            }
            Commands::Events { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("events", config, &stacks, args)
            }
            Commands::Exec { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("exec", config, &stacks, args)
            }
            Commands::Images { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("images", config, &stacks, args)
            }
            Commands::Kill { args } => {
                let stacks = reverse(config.stacks_with_dependants(globals.stacks())?);
                run_against_stacks("kill", config, &stacks, args)
            }
            Commands::Logs { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("logs", config, &stacks, args)
            }
            Commands::Pause { args } => {
                let stacks = reverse(config.stacks_with_dependants(globals.stacks())?);
                run_against_stacks("pause", config, &stacks, args)
            }
            Commands::Port { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("port", config, &stacks, args)
            }
            Commands::Ps { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("ps", config, &stacks, args)
            }
            Commands::Pull { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("pull", config, &stacks, args)
            }
            Commands::Push { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("push", config, &stacks, args)
            }
            Commands::Restart { args } => {
                let stacks = reverse(config.stacks_with_dependants(globals.stacks())?);
                run_against_stacks("down", config, &stacks, args)?;
                let stacks = reverse(stacks);
                let mut up_stacks = config.stacks_with_dependencies(globals.stacks())?;
                let first_keys: HashSet<String> = up_stacks.iter().map(|s| s.key.clone()).collect();
                up_stacks.extend(stacks.into_iter().filter(|s| !first_keys.contains(&s.key)));
                run_against_stacks("up", config, &up_stacks, &vec!["--wait".to_string()])
            }
            Commands::Rm { args } => {
                let stacks = reverse(config.stacks_with_dependants(globals.stacks())?);
                run_against_stacks("rm", config, &stacks, args)
            }
            Commands::Run { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("run", config, &stacks, args)
            }
            Commands::Start { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("start", config, &stacks, args)
            }
            Commands::Stop { args } => {
                let stacks = config.stack(globals.stacks())?;
                run_against_stacks("stop", config, &stacks, args)
            }
            Commands::Top { args } => {
                let stacks = config.stacks(globals.stacks())?;
                run_against_stacks("top", config, &stacks, args)
            }
            Commands::Unpause { args } => {
                let stacks = config.stacks_with_dependencies(globals.stacks())?;
                run_against_stacks("unpause", config, &stacks, args)
            }
            Commands::Up { args } => {
                let mut args = args.clone();
                args.insert(0, "--wait".to_string());
                let stacks = config.stacks_with_dependencies(globals.stacks())?;
                run_against_stacks("up", config, &stacks, &args)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Program;
    use clap::Parser;

    #[test]
    fn stacks() {
        let program = Program::parse_from(["stack", "up"]);
        assert_eq!(program.file, None);
        assert_eq!(program.globals.stacks(), Vec::<&str>::new());

        let program = Program::parse_from(["stack", "-f", "foo", "*", "down"]);
        assert_eq!(program.file, Some("foo".to_string()));
        assert_eq!(program.globals.stacks(), Vec::<&str>::new());

        let program = Program::parse_from(["stack", "bar", "up"]);
        assert_eq!(program.file, None);
        assert_eq!(program.globals.stacks(), vec!["bar"]);
    }
}
