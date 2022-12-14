use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{Config, Stack};

#[derive(Default, Clone)]
pub struct ExecOptions {
    pub binary: Vec<String>,
    pub global_args: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
    pub environment: HashMap<String, String>,
    pub working_dir: PathBuf,
}

impl ExecOptions {
    pub fn new<S: AsRef<str>>(config: &Config, command: &str, args: &[S]) -> Self {
        Self {
            binary: config.command.clone(),
            command: command.to_owned(),
            working_dir: config.base_dir.clone(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
            environment: config.environment.clone(),
            ..Default::default()
        }
    }

    pub fn with_stack(&self, stack: &Stack) -> Self {
        let mut options = self.clone();
        let project_directory = stack.directory(&self.working_dir);

        options.global_args.extend([
            "-p".to_string(),
            stack.name.clone(),
            "--project-directory".to_string(),
            project_directory.to_str().unwrap().to_string(),
        ]);

        if let Some(ref list) = stack.file {
            for file in list {
                options.global_args.extend([
                    "-f".to_string(),
                    self.working_dir.join(file).to_str().unwrap().to_string(),
                ])
            }
        }

        options.working_dir = project_directory;

        options.environment.extend(
            stack
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );

        options
    }

    pub fn program(&self) -> &str {
        self.binary.get(0).unwrap()
    }

    pub fn args(&self) -> Vec<&str> {
        let mut args: Vec<&str> = self
            .binary
            .iter()
            .skip(1)
            .chain(self.global_args.iter())
            .map(AsRef::<str>::as_ref)
            .collect();

        args.push(self.command.as_ref());

        args.extend(self.args.iter().map(AsRef::<str>::as_ref));

        args
    }
}
