//! A minimal client for the parts of the Portainer API needed to deploy stacks
//! and wait for them to become healthy.

use std::thread;
use std::time::{Duration, Instant};

use reqwest::{
    blocking::{Client as HttpClient, RequestBuilder},
    header::{HeaderMap, HeaderValue},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;

use crate::config::Endpoint;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct EndpointInfo {
    #[serde(rename = "Id")]
    id: u64,
    #[serde(rename = "Name")]
    name: String,
}

#[derive(Deserialize)]
pub struct StackInfo {
    #[serde(rename = "Id")]
    pub id: u64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Status")]
    pub status: u8,
    /// The history of deployments, only present in newer versions of
    /// Portainer which deploy asynchronously.
    #[serde(rename = "DeploymentStatus")]
    deployment_status: Option<Vec<DeploymentStatus>>,
}

#[derive(Deserialize)]
struct DeploymentStatus {
    #[serde(rename = "Status")]
    status: u8,
    #[serde(rename = "Message")]
    message: Option<String>,
}

impl StackInfo {
    /// Whether Portainer considers the stack to be stopped.
    fn is_stopped(&self) -> bool {
        self.status == 2
    }

    /// Checks the latest deployment, returning whether it has completed.
    fn is_deployed(&self) -> Result<bool, String> {
        let latest = self
            .deployment_status
            .as_ref()
            .and_then(|statuses| statuses.last());

        match latest {
            Some(DeploymentStatus { status: 3, .. }) => Ok(false),
            Some(DeploymentStatus {
                status: 4, message, ..
            }) => Err(format!(
                "Deploying stack \"{}\" failed: {}",
                self.name,
                message.as_deref().unwrap_or("unknown error")
            )),
            _ => Ok(true),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateStack<'a> {
    name: &'a str,
    stack_file_content: &'a str,
    env: [(); 0],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStack<'a> {
    stack_file_content: &'a str,
    env: [(); 0],
    prune: bool,
    pull_image: bool,
}

#[derive(Deserialize)]
struct ContainerSummary {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Deserialize)]
struct ContainerHealth {
    #[serde(rename = "Status")]
    status: String,
}

#[derive(Deserialize)]
struct ContainerStatus {
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "ExitCode", default)]
    exit_code: i64,
    #[serde(rename = "Health")]
    health: Option<ContainerHealth>,
}

#[derive(Deserialize)]
struct ContainerInspect {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "State")]
    state: ContainerStatus,
}

/// The state of a single container in a project.
#[derive(Debug)]
pub struct Container {
    name: String,
    state: String,
    exit_code: i64,
    health: Option<String>,
}

impl From<ContainerInspect> for Container {
    fn from(inspect: ContainerInspect) -> Self {
        Container {
            name: inspect.name.trim_start_matches('/').to_owned(),
            state: inspect.state.status,
            exit_code: inspect.state.exit_code,
            health: inspect.state.health.map(|h| h.status),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Ready,
    Waiting(Vec<String>),
    Failed(String),
}

/// Decides whether a project's containers are all up and healthy. Containers
/// that have exited successfully are considered complete, matching the
/// behaviour of `docker compose up --wait`.
pub fn evaluate(containers: &[Container]) -> Status {
    let mut waiting = Vec::new();

    for container in containers {
        match container.state.as_str() {
            "running" => match container.health.as_deref() {
                None | Some("healthy") => {}
                Some("unhealthy") => {
                    return Status::Failed(format!("container {} is unhealthy", container.name))
                }
                Some(health) => waiting.push(format!("{} ({})", container.name, health)),
            },
            "exited" | "dead" => {
                if container.exit_code != 0 {
                    return Status::Failed(format!(
                        "container {} exited with code {}",
                        container.name, container.exit_code
                    ));
                }
            }
            state => waiting.push(format!("{} ({})", container.name, state)),
        }
    }

    if waiting.is_empty() {
        Status::Ready
    } else {
        Status::Waiting(waiting)
    }
}

pub struct Client {
    url: String,
    http: HttpClient,
}

impl Client {
    pub fn new(url: &str, token: &str) -> Result<Self, String> {
        let mut token =
            HeaderValue::from_str(token).map_err(|e| format!("Invalid Portainer token: {}", e))?;
        token.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert("X-API-Key", token);

        let http = HttpClient::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| format!("Unable to create HTTP client: {}", e))?;

        Ok(Client {
            url: url.trim_end_matches('/').to_owned(),
            http,
        })
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/{}", self.url, path)
    }

    fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, String> {
        let response = request
            .send()
            .map_err(|e| format!("Portainer request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let url = response.url().clone();
            let body = response.text().unwrap_or_default();
            return Err(format!(
                "Portainer request to {} failed with {}: {}",
                url.path(),
                status,
                body.trim()
            ));
        }

        response
            .json()
            .map_err(|e| format!("Invalid response from Portainer: {}", e))
    }

    /// Finds the id of the given environment.
    pub fn resolve_endpoint(&self, endpoint: &Endpoint) -> Result<u64, String> {
        let endpoints: Vec<EndpointInfo> = self.send(self.http.get(self.api("endpoints")))?;

        let matching: Vec<&EndpointInfo> = endpoints
            .iter()
            .filter(|info| match endpoint {
                Endpoint::Id(id) => info.id == *id,
                Endpoint::Name(name) => info.name == *name,
            })
            .collect();

        match matching.as_slice() {
            [info] => Ok(info.id),
            [] => Err(format!("Unknown Portainer environment \"{}\"", endpoint)),
            _ => Err(format!(
                "Multiple Portainer environments are named \"{}\", use its id instead",
                endpoint
            )),
        }
    }

    pub fn find_stack(&self, endpoint_id: u64, name: &str) -> Result<Option<StackInfo>, String> {
        let filters = json!({ "EndpointID": endpoint_id }).to_string();
        let stacks: Vec<StackInfo> = self.send(
            self.http
                .get(self.api("stacks"))
                .query(&[("filters", filters)]),
        )?;

        Ok(stacks.into_iter().find(|stack| stack.name == name))
    }

    pub fn create_stack(
        &self,
        endpoint_id: u64,
        name: &str,
        content: &str,
    ) -> Result<StackInfo, String> {
        self.send(
            self.http
                .post(self.api("stacks/create/standalone/string"))
                .query(&[("endpointId", endpoint_id)])
                .json(&CreateStack {
                    name,
                    stack_file_content: content,
                    env: [],
                }),
        )
    }

    /// Updates the stack's compose file and redeploys it, starting it if it
    /// was stopped.
    pub fn update_stack(
        &self,
        endpoint_id: u64,
        stack: &StackInfo,
        content: &str,
        pull: bool,
    ) -> Result<StackInfo, String> {
        let updated: StackInfo = self.send(
            self.http
                .put(self.api(&format!("stacks/{}", stack.id)))
                .query(&[("endpointId", endpoint_id)])
                .json(&UpdateStack {
                    stack_file_content: content,
                    env: [],
                    prune: true,
                    pull_image: pull,
                }),
        )?;

        if updated.is_stopped() {
            tracing::debug!("Starting stopped stack \"{}\"", updated.name);
            self.send(
                self.http
                    .post(self.api(&format!("stacks/{}/start", updated.id)))
                    .query(&[("endpointId", endpoint_id)]),
            )
        } else {
            Ok(updated)
        }
    }

    fn project_containers(
        &self,
        endpoint_id: u64,
        project: &str,
    ) -> Result<Vec<Container>, String> {
        let docker = format!("endpoints/{}/docker", endpoint_id);
        let filters = json!({
            "label": [
                format!("com.docker.compose.project={}", project),
                "com.docker.compose.oneoff=False",
            ]
        })
        .to_string();

        let summaries: Vec<ContainerSummary> = self.send(
            self.http
                .get(self.api(&format!("{}/containers/json", docker)))
                .query(&[("all", "true"), ("filters", &filters)]),
        )?;

        summaries
            .iter()
            .map(|summary| {
                let inspect: ContainerInspect = self.send(
                    self.http
                        .get(self.api(&format!("{}/containers/{}/json", docker, summary.id))),
                )?;
                Ok(inspect.into())
            })
            .collect()
    }

    /// Waits for Portainer to finish deploying the stack and then for all of
    /// its containers to be running and healthy.
    pub fn wait_ready(
        &self,
        endpoint_id: u64,
        stack: StackInfo,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let project = stack.name.clone();

        let mut current = stack;
        while !current.is_deployed()? {
            if Instant::now() >= deadline {
                return Err(format!(
                    "Timed out waiting for Portainer to deploy stack \"{}\"",
                    project
                ));
            }

            tracing::debug!("Waiting for Portainer to deploy stack \"{}\"", project);
            thread::sleep(POLL_INTERVAL);
            current = self.send(
                self.http
                    .get(self.api(&format!("stacks/{}", current.id)))
                    .query(&[("endpointId", endpoint_id)]),
            )?;
        }

        loop {
            match evaluate(&self.project_containers(endpoint_id, &project)?) {
                Status::Ready => return Ok(()),
                Status::Failed(reason) => {
                    return Err(format!("Stack \"{}\" failed: {}", project, reason))
                }
                Status::Waiting(containers) => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "Timed out waiting for stack \"{}\": {}",
                            project,
                            containers.join(", ")
                        ));
                    }

                    tracing::debug!("Waiting for {}", containers.join(", "));
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{evaluate, Container, Status};

    fn container(state: &str, exit_code: i64, health: Option<&str>) -> Container {
        Container {
            name: "app".to_string(),
            state: state.to_string(),
            exit_code,
            health: health.map(str::to_string),
        }
    }

    #[test]
    fn health() {
        assert_eq!(evaluate(&[]), Status::Ready);
        assert_eq!(
            evaluate(&[
                container("running", 0, None),
                container("running", 0, Some("healthy")),
                container("exited", 0, None),
            ]),
            Status::Ready
        );
        assert_eq!(
            evaluate(&[
                container("running", 0, None),
                container("running", 0, Some("starting")),
            ]),
            Status::Waiting(vec!["app (starting)".to_string()])
        );
        assert_eq!(
            evaluate(&[container("created", 0, None)]),
            Status::Waiting(vec!["app (created)".to_string()])
        );
        assert_eq!(
            evaluate(&[container("running", 0, Some("unhealthy"))]),
            Status::Failed("container app is unhealthy".to_string())
        );
        assert_eq!(
            evaluate(&[container("exited", 1, None)]),
            Status::Failed("container app exited with code 1".to_string())
        );
    }
}
