use std::{thread, time::Duration};

use crate::{
    config::{Config, Stack},
    exec::ExecOptions,
    infer::{compose_config, ComposeProject},
    portainer::Client,
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

/// Loads the resolved compose configuration for a stack, checking that it can
/// be deployed through Portainer.
fn deployable_config(config: &Config, stack: &Stack) -> Result<String, String> {
    let yaml = compose_config(config, stack)
        .map_err(|e| format!("Unable to load stack \"{}\": {}", stack.key, e))?;

    let project = ComposeProject::parse(&yaml)
        .map_err(|e| format!("Unable to parse stack \"{}\": {}", stack.key, e))?;
    let built = project.built_services();
    if !built.is_empty() {
        return Err(format!(
            "Stack \"{}\" cannot be deployed as it builds services from source: {}",
            stack.key,
            built.join(", ")
        ));
    }

    String::from_utf8(yaml)
        .map_err(|e| format!("Invalid configuration for stack \"{}\": {}", stack.key, e))
}

pub fn deploy_stacks(
    config: &Config,
    stacks: &[&Stack],
    token: &str,
    pull: bool,
    timeout: Option<u64>,
) -> Result<(), String> {
    let portainer = config
        .portainer
        .as_ref()
        .ok_or("No portainer configuration is present in the stacks file.")?;
    let timeout = Duration::from_secs(timeout.unwrap_or(portainer.timeout));

    let client = Client::new(&portainer.url, token)?;
    let endpoint_id = client.resolve_endpoint(&portainer.endpoint)?;

    // Load everything up front so that we fail before deploying anything.
    let configs: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = stacks
            .iter()
            .map(|stack| scope.spawn(|| deployable_config(config, stack)))
            .collect();

        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("Failed to load stack configuration".to_string()))
            })
            .collect::<Result<_, _>>()
    })?;

    for (stack, content) in stacks.iter().zip(configs) {
        let deployed = match client.find_stack(endpoint_id, &stack.name)? {
            Some(existing) => {
                tracing::info!("Updating stack \"{}\"", stack.name);
                client.update_stack(endpoint_id, &existing, &content, pull)?
            }
            None => {
                tracing::info!("Creating stack \"{}\"", stack.name);
                client.create_stack(endpoint_id, &stack.name, &content)?
            }
        };

        tracing::info!("Waiting for stack \"{}\" to become healthy", stack.name);
        client.wait_ready(endpoint_id, deployed, timeout)?;
    }

    Ok(())
}
