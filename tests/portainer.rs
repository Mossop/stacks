//! Integration tests that deploy stacks to a real Portainer instance. These need
//! a Docker daemon so are ignored by default. Run them with
//! `cargo test --test portainer -- --ignored`.

use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{self, Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use reqwest::blocking::{multipart::Form, Client};
use serde_json::{json, Value};
use testcontainers::{
    core::{wait::HttpWaitStrategy, IntoContainerPort, Mount, WaitFor},
    runners::SyncRunner,
    Container, GenericImage, ImageExt,
};

const PORTAINER_IMAGE: &str = "portainer/portainer-ce";
const PORTAINER_TAG: &str = "2.45.3";
const PASSWORD: &str = "stacks-test-password";

/// A throwaway Portainer instance managing the local Docker daemon.
struct Portainer {
    _container: Container<GenericImage>,
    url: String,
    token: String,
    endpoint_id: u64,
    http: Client,
}

impl Portainer {
    fn start() -> Self {
        let container = GenericImage::new(PORTAINER_IMAGE, PORTAINER_TAG)
            .with_exposed_port(9000.tcp())
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/api/system/status")
                    .with_port(9000.tcp())
                    .with_expected_status_code(200u16),
            ))
            .with_mount(Mount::bind_mount(
                "/var/run/docker.sock",
                "/var/run/docker.sock",
            ))
            .with_cmd(["--no-setup-token"])
            .start()
            .expect("Failed to start Portainer");

        let port = container.get_host_port_ipv4(9000).unwrap();
        let url = format!("http://127.0.0.1:{}", port);
        let http = Client::builder().no_proxy().build().unwrap();

        let send = |request: reqwest::blocking::RequestBuilder| -> Value {
            let response = request.send().unwrap();
            let status = response.status();
            let body: Value = response.json().unwrap();
            assert!(status.is_success(), "Portainer setup failed: {}", body);
            body
        };

        send(
            http.post(format!("{}/api/users/admin/init", url))
                .json(&json!({
                    "Username": "admin",
                    "Password": PASSWORD,
                })),
        );

        let auth = send(http.post(format!("{}/api/auth", url)).json(&json!({
            "Username": "admin",
            "Password": PASSWORD,
        })));
        let jwt = auth["jwt"].as_str().unwrap();

        let endpoint = send(
            http.post(format!("{}/api/endpoints", url))
                .bearer_auth(jwt)
                .multipart(
                    Form::new()
                        .text("Name", "local")
                        .text("EndpointCreationType", "1"),
                ),
        );

        let token = send(
            http.post(format!("{}/api/users/1/tokens", url))
                .bearer_auth(jwt)
                .json(&json!({
                    "description": "stacks tests",
                    "password": PASSWORD,
                })),
        );

        Portainer {
            _container: container,
            url,
            token: token["rawAPIKey"].as_str().unwrap().to_owned(),
            endpoint_id: endpoint["Id"].as_u64().unwrap(),
            http,
        }
    }

    fn get(&self, path: &str) -> Value {
        self.get_with(path, &[])
    }

    fn get_with(&self, path: &str, query: &[(&str, &str)]) -> Value {
        self.http
            .get(format!("{}/api/{}", self.url, path))
            .query(query)
            .header("X-API-Key", &self.token)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap()
    }

    fn post(&self, path: &str) {
        self.http
            .post(format!("{}/api/{}", self.url, path))
            .header("X-API-Key", &self.token)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    fn stacks(&self) -> Vec<Value> {
        self.get("stacks").as_array().unwrap().clone()
    }

    fn stack(&self, name: &str) -> Option<Value> {
        self.stacks()
            .into_iter()
            .find(|stack| stack["Name"] == name)
    }

    fn stop_stack(&self, name: &str) {
        let stack = self.stack(name).unwrap();
        self.post(&format!(
            "stacks/{}/stop?endpointId={}",
            stack["Id"], self.endpoint_id
        ));
    }

    /// Inspects a container through Portainer's docker proxy.
    fn container(&self, name: &str) -> Value {
        self.get(&format!(
            "endpoints/{}/docker/containers/{}/json",
            self.endpoint_id, name
        ))
    }

    /// Lists the compose projects of every container created from the
    /// fixture's services, whichever project or stack they belong to.
    fn compose_projects(&self, fixture: &Fixture) -> BTreeSet<String> {
        let filters = json!({
            "label": [format!("stacks.test={}", fixture.prefix)]
        })
        .to_string();

        self.get_with(
            &format!("endpoints/{}/docker/containers/json", self.endpoint_id),
            &[("all", "1"), ("filters", &filters)],
        )
        .as_array()
        .unwrap()
        .iter()
        .map(|container| {
            container["Labels"]["com.docker.compose.project"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
    }
}

impl Drop for Portainer {
    fn drop(&mut self) {
        // Remove the stacks so their containers and networks don't outlive
        // Portainer on the shared Docker daemon. Dependants are removed first
        // so that networks are no longer in use when their stack is removed.
        let mut stacks = self.stacks();
        stacks.sort_by_key(|stack| std::cmp::Reverse(stack["Id"].as_u64()));

        let deadline = Instant::now() + Duration::from_secs(60);
        for stack in stacks {
            let _ = self
                .http
                .delete(format!(
                    "{}/api/stacks/{}?endpointId={}",
                    self.url, stack["Id"], self.endpoint_id
                ))
                .header("X-API-Key", &self.token)
                .send();

            // Portainer removes stacks asynchronously so wait for that to
            // finish.
            let filters = json!({
                "label": [format!("com.docker.compose.project={}", stack["Name"].as_str().unwrap())]
            })
            .to_string();
            let docker = format!("endpoints/{}/docker", self.endpoint_id);
            while Instant::now() < deadline {
                let containers = self.get_with(
                    &format!("{}/containers/json", docker),
                    &[("all", "1"), ("filters", &filters)],
                );
                let networks =
                    self.get_with(&format!("{}/networks", docker), &[("filters", &filters)]);
                if containers.as_array().unwrap().is_empty()
                    && networks.as_array().unwrap().is_empty()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// A stacks configuration in a temporary directory. Stack names are prefixed
/// so that tests running in parallel don't collide on the Docker daemon.
struct Fixture {
    dir: PathBuf,
    prefix: String,
}

impl Fixture {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let prefix = format!(
            "t{}x{}",
            process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let dir = std::env::temp_dir().join(format!("stacks-{}", prefix));
        fs::create_dir_all(&dir).unwrap();

        Fixture { dir, prefix }
    }

    /// The docker name of the given stack.
    fn name(&self, stack: &str) -> String {
        format!("{}{}", self.prefix, stack)
    }

    /// The name of the first container for a service in a stack.
    fn container(&self, stack: &str, service: &str) -> String {
        format!("{}-{}-1", self.name(stack), service)
    }

    /// The name of a network shared between stacks.
    fn network(&self) -> String {
        format!("{}_shared", self.prefix)
    }

    /// Writes the stacks config for the given stack keys.
    fn config(&self, portainer: &Portainer, endpoint: &str, stacks: &[&str]) {
        let mut config = format!(
            "portainer:\n  url: {}\n  endpoint: {}\nstacks:\n",
            portainer.url, endpoint
        );
        for stack in stacks {
            config.push_str(&format!("  {}:\n    name: {}\n", stack, self.name(stack)));
        }

        fs::write(self.dir.join("stacks.yml"), config).unwrap();
    }

    fn compose(&self, stack: &str, content: &str) {
        let dir = self.dir.join(stack);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("compose.yml"), content).unwrap();
    }

    fn run(&self, portainer: &Portainer, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_stacks"))
            .args(args)
            .current_dir(&self.dir)
            .env("PORTAINER_TOKEN", &portainer.token)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    /// A busybox service that becomes healthy after `delay` seconds, or never
    /// if `delay` is `None`. Its containers are labelled with this fixture's
    /// prefix so they can be found whatever compose project they end up in.
    fn service(&self, name: &str, delay: Option<u32>) -> String {
        let (command, retries) = match delay {
            Some(delay) => (format!("sleep {} && touch /ok && sleep 3600", delay), 60),
            None => ("sleep 3600".to_string(), 2),
        };
        let prefix = &self.prefix;

        format!(
            r#"
  {name}:
    image: busybox
    command: ["sh", "-c", "{command}"]
    stop_signal: SIGKILL
    labels:
      stacks.test: {prefix}
    healthcheck:
      test: ["CMD", "test", "-f", "/ok"]
      interval: 1s
      retries: {retries}
"#
        )
    }
}

/// Writes a `net` stack that creates the shared network and an `app` stack
/// that uses it, so `app` is inferred to depend on `net`.
fn dependent_stacks(fixture: &Fixture, net_delay: Option<u32>) {
    fixture.compose(
        "net",
        &format!(
            "services:{}networks:\n  default:\n    name: {}\n",
            fixture.service("hub", net_delay),
            fixture.network()
        ),
    );
    fixture.compose(
        "app",
        &format!(
            "services:{}networks:\n  default:\n    name: {}\n    external: true\n",
            fixture.service("web", Some(0)),
            fixture.network()
        ),
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "stacks failed: {}", stderr(output));
}

fn assert_failure(output: &Output, message: &str) {
    let stderr = stderr(output);
    assert!(!output.status.success(), "stacks unexpectedly succeeded");
    assert!(
        stderr.contains(message),
        "expected \"{}\" in output: {}",
        message,
        stderr
    );
}

/// Normalises a docker timestamp so that timestamps compare as strings.
fn timestamp(value: &Value) -> String {
    let value = value.as_str().unwrap().trim_end_matches('Z');
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    format!("{}.{:0<9}", seconds, fraction)
}

#[test]
#[ignore = "requires docker"]
fn deploy_creates_then_updates() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["net", "app"]);
    dependent_stacks(&fixture, Some(3));

    let output = fixture.run(&portainer, &["deploy"]);
    assert_success(&output);
    let log = stderr(&output);
    assert!(log.contains(&format!("Creating stack \"{}\"", fixture.name("net"))));
    assert!(log.contains(&format!("Creating stack \"{}\"", fixture.name("app"))));

    let net = portainer.stack(&fixture.name("net")).unwrap();
    let app = portainer.stack(&fixture.name("app")).unwrap();
    assert_eq!(net["EndpointId"], portainer.endpoint_id);
    assert_eq!(app["EndpointId"], portainer.endpoint_id);

    // The dependency must have been healthy before the dependant was created.
    let hub = portainer.container(&fixture.container("net", "hub"));
    let web = portainer.container(&fixture.container("app", "web"));
    assert_eq!(hub["State"]["Health"]["Status"], "healthy");
    assert_eq!(web["State"]["Status"], "running");
    let healthy_at = hub["State"]["Health"]["Log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["ExitCode"] == 0)
        .map(|check| timestamp(&check["End"]))
        .unwrap();
    assert!(healthy_at < timestamp(&web["Created"]));

    // Deploying again updates the existing stacks.
    let output = fixture.run(&portainer, &["deploy"]);
    assert_success(&output);
    let log = stderr(&output);
    assert!(log.contains(&format!("Updating stack \"{}\"", fixture.name("net"))));
    assert!(log.contains(&format!("Updating stack \"{}\"", fixture.name("app"))));
    assert_eq!(portainer.stacks().len(), 2);
    assert_eq!(
        portainer.stack(&fixture.name("net")).unwrap()["Id"],
        net["Id"]
    );
    assert_eq!(
        portainer.stack(&fixture.name("app")).unwrap()["Id"],
        app["Id"]
    );
}

#[test]
#[ignore = "requires docker"]
fn deploy_starts_stopped_stack() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["app"]);
    fixture.compose(
        "app",
        &format!("services:{}", fixture.service("web", Some(0))),
    );

    assert_success(&fixture.run(&portainer, &["deploy"]));

    portainer.stop_stack(&fixture.name("app"));
    assert_eq!(portainer.stack(&fixture.name("app")).unwrap()["Status"], 2);

    assert_success(&fixture.run(&portainer, &["deploy"]));
    assert_eq!(portainer.stack(&fixture.name("app")).unwrap()["Status"], 1);
    let web = portainer.container(&fixture.container("app", "web"));
    assert_eq!(web["State"]["Health"]["Status"], "healthy");
}

#[test]
#[ignore = "requires docker"]
fn unhealthy_stack_stops_deploy() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["net", "app"]);
    dependent_stacks(&fixture, None);

    let output = fixture.run(&portainer, &["deploy"]);
    assert_failure(
        &output,
        &format!("container {} is unhealthy", fixture.container("net", "hub")),
    );
    assert!(portainer.stack(&fixture.name("net")).is_some());
    assert!(portainer.stack(&fixture.name("app")).is_none());
}

#[test]
#[ignore = "requires docker"]
fn failed_container_stops_deploy() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["job"]);
    fixture.compose(
        "job",
        "services:\n  task:\n    image: busybox\n    command: [\"sh\", \"-c\", \"exit 3\"]\n",
    );

    let output = fixture.run(&portainer, &["deploy"]);
    assert_failure(
        &output,
        &format!(
            "container {} exited with code 3",
            fixture.container("job", "task")
        ),
    );
}

#[test]
#[ignore = "requires docker"]
fn build_is_rejected_before_deploying() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["net", "app"]);
    dependent_stacks(&fixture, Some(0));
    fixture.compose(
        "app",
        &format!(
            "services:{}  builder:\n    build: .\nnetworks:\n  default:\n    name: {}\n    external: true\n",
            fixture.service("web", Some(0)),
            fixture.network()
        ),
    );

    let output = fixture.run(&portainer, &["deploy"]);
    assert_failure(
        &output,
        "Stack \"app\" cannot be deployed as it builds services from source: builder",
    );
    assert!(portainer.stacks().is_empty());
}

#[test]
#[ignore = "requires docker"]
fn endpoint_by_id_and_unknown_endpoint() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.compose(
        "app",
        &format!("services:{}", fixture.service("web", Some(0))),
    );

    fixture.config(&portainer, "unknown", &["app"]);
    let output = fixture.run(&portainer, &["deploy"]);
    assert_failure(&output, "Unknown Portainer environment \"unknown\"");
    assert!(portainer.stacks().is_empty());

    fixture.config(&portainer, &portainer.endpoint_id.to_string(), &["app"]);
    assert_success(&fixture.run(&portainer, &["deploy"]));
    let app = portainer.stack(&fixture.name("app")).unwrap();
    assert_eq!(app["EndpointId"], portainer.endpoint_id);
}

#[test]
#[ignore = "requires docker"]
fn timeout() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["app"]);
    fixture.compose(
        "app",
        &format!("services:{}", fixture.service("web", Some(60))),
    );

    let output = fixture.run(&portainer, &["deploy", "--timeout", "3"]);
    assert_failure(
        &output,
        &format!("Timed out waiting for stack \"{}\"", fixture.name("app")),
    );
}

/// Checks that the only Portainer stack and the only compose project are the
/// given stack, with a single running container for its service. Whether
/// docker compose reuses or recreates the container when switching between
/// `up` and `deploy` depends on its version so that isn't checked.
fn assert_single_stack(portainer: &Portainer, fixture: &Fixture, stack: &str) {
    let name = fixture.name(stack);

    let stacks: Vec<Value> = portainer.stacks();
    assert_eq!(
        stacks.len(),
        1,
        "expected one Portainer stack: {:?}",
        stacks
    );
    assert_eq!(stacks[0]["Name"], name);

    assert_eq!(
        portainer.compose_projects(fixture),
        BTreeSet::from([name.clone()])
    );

    let web = portainer.container(&fixture.container(stack, "web"));
    assert_eq!(web["State"]["Health"]["Status"], "healthy");
}

#[test]
#[ignore = "requires docker"]
fn up_after_deploy_modifies_stack() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["app"]);
    fixture.compose(
        "app",
        &format!("services:{}", fixture.service("web", Some(0))),
    );

    assert_success(&fixture.run(&portainer, &["deploy"]));
    assert_single_stack(&portainer, &fixture, "app");
    let projects = portainer.compose_projects(&fixture);

    assert_success(&fixture.run(&portainer, &["up"]));
    assert_single_stack(&portainer, &fixture, "app");
    assert_eq!(portainer.compose_projects(&fixture), projects);
}

#[test]
#[ignore = "requires docker"]
fn deploy_after_up_updates_stack() {
    let portainer = Portainer::start();
    let fixture = Fixture::new();
    fixture.config(&portainer, "local", &["app"]);
    fixture.compose(
        "app",
        &format!("services:{}", fixture.service("web", Some(0))),
    );

    assert_success(&fixture.run(&portainer, &["up"]));
    assert!(portainer.stacks().is_empty());
    let projects = portainer.compose_projects(&fixture);
    assert_eq!(projects, BTreeSet::from([fixture.name("app")]));

    assert_success(&fixture.run(&portainer, &["deploy"]));
    assert_single_stack(&portainer, &fixture, "app");
    assert_eq!(portainer.compose_projects(&fixture), projects);
}
