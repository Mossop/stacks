use crate::{
    config::{Config, Stack},
    exec::ExecOptions,
};

fn exec(exec_options: &ExecOptions, stack: &Stack) -> Result<(), String> {
    let exec_options = exec_options.with_stack(stack);

    tracing::debug!(
        "Executing `{} {}`",
        exec_options.program(),
        exec_options.args().join(" ")
    );

    let mut child = exec_options
        .command()
        .spawn()
        .map_err(|e| format!("Error running docker compose: {}", e))?;
    let status = child
        .wait()
        .map_err(|e| format!("Error running docker compose: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Error running command `{} {}`: {}",
            exec_options.program(),
            exec_options.args().join(" "),
            status
        ))
    }
}

pub fn run_against_stacks(
    command: &str,
    config: &Config,
    stacks: &Vec<&Stack>,
    args: &Vec<String>,
) -> Result<(), String> {
    tracing::trace!(
        "Executing command `{}` against {} stacks with arguments {:?}",
        command,
        stacks
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<String>>()
            .join(","),
        args
    );

    let exec_options = ExecOptions::new(config, command, args);
    for stack in stacks {
        exec(&exec_options, stack)?;
    }

    Ok(())
}
