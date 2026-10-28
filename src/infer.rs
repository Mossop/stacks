//! Infers dependencies between stacks by examining the resolved docker compose
//! configuration of each stack. A stack that references an external network or
//! volume, or a container by name, depends on whichever stack defines it.

use std::collections::{BTreeMap, BTreeSet};
use std::thread;

use serde::Deserialize;

use crate::{
    config::{Config, Stack},
    exec::ExecOptions,
};

#[derive(Deserialize, Default)]
struct Resource {
    name: Option<String>,
    #[serde(default)]
    external: bool,
}

#[derive(Deserialize, Default)]
struct Service {
    container_name: Option<String>,
    network_mode: Option<String>,
    #[serde(default)]
    volumes_from: Vec<String>,
    #[serde(default)]
    external_links: Vec<String>,
    build: Option<serde_yaml_ng::Value>,
}

/// The parts of the output of `docker compose config` that we care about.
#[derive(Deserialize, Default)]
pub struct ComposeProject {
    #[serde(default)]
    name: String,
    #[serde(default)]
    services: BTreeMap<String, Service>,
    #[serde(default)]
    networks: BTreeMap<String, Resource>,
    #[serde(default)]
    volumes: BTreeMap<String, Resource>,
}

/// Named docker resources that a project either defines or references.
#[derive(Default, Debug, PartialEq)]
struct Resources {
    networks: BTreeSet<String>,
    volumes: BTreeSet<String>,
    containers: BTreeSet<String>,
}

impl Resources {
    fn intersects(&self, other: &Resources) -> bool {
        !self.networks.is_disjoint(&other.networks)
            || !self.volumes.is_disjoint(&other.volumes)
            || !self.containers.is_disjoint(&other.containers)
    }
}

/// Strips the `container:` prefix from a reference, returning the container
/// name if present.
fn container_reference(reference: &str) -> Option<String> {
    let name = reference.strip_prefix("container:")?;
    // volumes_from may include an access mode suffix.
    let name = name
        .strip_suffix(":ro")
        .or_else(|| name.strip_suffix(":rw"))
        .unwrap_or(name);
    Some(name.to_owned())
}

impl ComposeProject {
    pub fn parse(yaml: &[u8]) -> Result<Self, String> {
        serde_yaml_ng::from_slice(yaml).map_err(|e| e.to_string())
    }

    /// The names of any services that are built from source rather than
    /// using a pre-built image.
    pub fn built_services(&self) -> Vec<&str> {
        self.services
            .iter()
            .filter(|(_, service)| service.build.is_some())
            .map(|(key, _)| key.as_str())
            .collect()
    }

    fn resource_name(key: &str, resource: &Resource) -> String {
        resource.name.clone().unwrap_or_else(|| key.to_owned())
    }

    /// The resources this project creates and so may provide to others.
    fn provides(&self) -> Resources {
        let mut resources = Resources::default();

        for (key, network) in self.networks.iter().filter(|(_, r)| !r.external) {
            resources.networks.insert(Self::resource_name(key, network));
        }

        for (key, volume) in self.volumes.iter().filter(|(_, r)| !r.external) {
            resources.volumes.insert(Self::resource_name(key, volume));
        }

        for (key, service) in self.services.iter() {
            match service.container_name {
                Some(ref name) => resources.containers.insert(name.clone()),
                None => resources
                    .containers
                    .insert(format!("{}-{}-1", self.name, key)),
            };
        }

        resources
    }

    /// The resources this project expects to already exist.
    fn requires(&self) -> Resources {
        let mut resources = Resources::default();

        for (key, network) in self.networks.iter().filter(|(_, r)| r.external) {
            resources.networks.insert(Self::resource_name(key, network));
        }

        for (key, volume) in self.volumes.iter().filter(|(_, r)| r.external) {
            resources.volumes.insert(Self::resource_name(key, volume));
        }

        for service in self.services.values() {
            if let Some(name) = service
                .network_mode
                .as_deref()
                .and_then(container_reference)
            {
                resources.containers.insert(name);
            }

            for name in service
                .volumes_from
                .iter()
                .filter_map(|r| container_reference(r))
            {
                resources.containers.insert(name);
            }

            for link in service.external_links.iter() {
                let name = link.split(':').next().unwrap_or(link);
                resources.containers.insert(name.to_owned());
            }
        }

        resources
    }
}

/// Given the compose configuration of each stack returns the inferred
/// dependencies as a map from stack key to the keys of the stacks it depends
/// on.
pub fn infer_dependencies(
    projects: &BTreeMap<String, ComposeProject>,
) -> BTreeMap<String, BTreeSet<String>> {
    let provided: BTreeMap<&String, Resources> = projects
        .iter()
        .map(|(key, project)| (key, project.provides()))
        .collect();

    let mut dependencies = BTreeMap::new();

    for (key, project) in projects.iter() {
        let requirements = project.requires();

        let deps: BTreeSet<String> = provided
            .iter()
            .filter(|(other, resources)| **other != key && requirements.intersects(resources))
            .map(|(other, _)| (*other).clone())
            .collect();

        if !deps.is_empty() {
            dependencies.insert(key.clone(), deps);
        }
    }

    dependencies
}

/// Runs `docker compose config` for the stack returning the resolved
/// configuration.
pub fn compose_config(config: &Config, stack: &Stack) -> Result<Vec<u8>, String> {
    let exec_options = ExecOptions::new(config, "config", &[] as &[&str]).with_stack(stack);

    let output = exec_options.command().output().map_err(|e| e.to_string())?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn load_project(config: &Config, stack: &Stack) -> Result<ComposeProject, String> {
    ComposeProject::parse(&compose_config(config, stack)?)
}

/// Runs `docker compose config` for every stack and infers dependencies from
/// the results. Stacks whose configuration cannot be loaded are skipped.
pub fn infer_config_dependencies(config: &Config) -> BTreeMap<String, BTreeSet<String>> {
    let projects: BTreeMap<String, ComposeProject> = thread::scope(|scope| {
        let handles: Vec<_> = config
            .stacks
            .values()
            .map(|stack| (stack, scope.spawn(|| load_project(config, stack))))
            .collect();

        handles
            .into_iter()
            .filter_map(|(stack, handle)| match handle.join() {
                Ok(Ok(project)) => Some((stack.key.clone(), project)),
                Ok(Err(e)) => {
                    tracing::warn!(
                        "Unable to infer dependencies for stack \"{}\": {}",
                        stack.key,
                        e
                    );
                    None
                }
                Err(_) => None,
            })
            .collect()
    });

    infer_dependencies(&projects)
}

#[cfg(test)]
mod tests {
    use super::{infer_dependencies, ComposeProject};
    use std::collections::{BTreeMap, BTreeSet};

    fn projects(list: &[(&str, &str)]) -> BTreeMap<String, ComposeProject> {
        list.iter()
            .map(|(key, yaml)| {
                (
                    key.to_string(),
                    ComposeProject::parse(yaml.as_bytes()).unwrap(),
                )
            })
            .collect()
    }

    fn deps(list: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        list.iter()
            .map(|(key, deps)| {
                (
                    key.to_string(),
                    deps.iter().map(|d| d.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn networks() {
        let projects = projects(&[
            (
                "networks",
                "
                name: networks
                services:
                    dummy:
                        image: busybox
                networks:
                    shared:
                        name: shared_net
                    default:
                        name: networks_default
                ",
            ),
            (
                "web",
                "
                name: web
                services:
                    web:
                        image: nginx
                networks:
                    ext:
                        name: shared_net
                        external: true
                    default:
                        name: web_default
                ",
            ),
            (
                "other",
                "
                name: other
                networks:
                    unknown:
                        name: unknown_net
                        external: true
                ",
            ),
        ]);

        assert_eq!(
            infer_dependencies(&projects),
            deps(&[("web", &["networks"])])
        );
    }

    #[test]
    fn volumes() {
        let projects = projects(&[
            (
                "data",
                "
                name: data
                volumes:
                    store:
                        name: data_store
                ",
            ),
            (
                "app",
                "
                name: app
                volumes:
                    store:
                        name: data_store
                        external: true
                ",
            ),
        ]);

        assert_eq!(infer_dependencies(&projects), deps(&[("app", &["data"])]));
    }

    #[test]
    fn containers() {
        let projects = projects(&[
            (
                "vpn",
                "
                name: vpn
                services:
                    vpn:
                        image: vpn
                        container_name: gateway
                    db:
                        image: postgres
                ",
            ),
            (
                "storage",
                "
                name: storage
                services:
                    data:
                        image: busybox
                ",
            ),
            (
                "app",
                "
                name: app
                services:
                    app:
                        image: app
                        network_mode: container:gateway
                ",
            ),
            (
                "backup",
                "
                name: backup
                services:
                    backup:
                        image: backup
                        volumes_from:
                            - container:storage-data-1:ro
                            - backup2
                    backup2:
                        image: backup
                ",
            ),
            (
                "web",
                "
                name: web
                services:
                    web:
                        image: nginx
                        external_links:
                            - vpn-db-1:database
                ",
            ),
        ]);

        assert_eq!(
            infer_dependencies(&projects),
            deps(&[
                ("app", &["vpn"]),
                ("backup", &["storage"]),
                ("web", &["vpn"]),
            ])
        );
    }

    #[test]
    fn self_references() {
        let projects = projects(&[(
            "app",
            "
            name: app
            services:
                app:
                    image: app
                    container_name: app
                sidecar:
                    image: sidecar
                    network_mode: container:app
            ",
        )]);

        assert_eq!(infer_dependencies(&projects), deps(&[]));
    }

    #[test]
    fn built_services() {
        let project = ComposeProject::parse(
            b"
            name: app
            services:
                app:
                    build:
                        context: .
                db:
                    image: postgres
                worker:
                    build: ./worker
            ",
        )
        .unwrap();

        assert_eq!(project.built_services(), vec!["app", "worker"]);
    }
}
