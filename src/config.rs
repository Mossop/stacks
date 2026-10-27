use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::de::{self, Error, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_with::formats::SpaceSeparator;
use serde_with::{serde_as, StringWithSeparator};

use crate::infer;

fn deserialize_file<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct FileVisitor {}
    impl<'de> Visitor<'de> for FileVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or a non-empty list of strings")
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Option<Vec<String>>, S::Error>
        where
            S: SeqAccess<'de>,
        {
            let mut list = Vec::new();
            while let Some(s) = seq.next_element()? {
                list.push(s);
            }

            if list.is_empty() {
                Err(S::Error::invalid_length(0, &self))
            } else {
                Ok(Some(list))
            }
        }

        fn visit_str<E>(self, s: &str) -> Result<Option<Vec<String>>, E>
        where
            E: de::Error,
        {
            Ok(Some(vec![s.to_owned()]))
        }

        fn visit_none<E>(self) -> Result<Option<Vec<String>>, E>
        where
            E: de::Error,
        {
            Ok(None)
        }
    }

    deserializer.deserialize_any(FileVisitor {})
}

/// How a dependency between two stacks came to be known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    /// Listed in `depends_on` in the stacks config.
    Explicit,
    /// Inferred from the docker compose configuration.
    Inferred,
}

impl fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DependencyKind::Explicit => f.write_str("explicit"),
            DependencyKind::Inferred => f.write_str("inferred"),
        }
    }
}

fn deserialize_dependencies<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, DependencyKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let keys: BTreeSet<String> = BTreeSet::deserialize(deserializer)?;
    Ok(keys
        .into_iter()
        .map(|key| (key, DependencyKind::Explicit))
        .collect())
}

#[serde_as]
#[derive(Deserialize, Eq)]
pub struct Stack {
    #[serde(skip)]
    pub key: String,
    #[serde(default)]
    pub name: String,
    pub directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_file")]
    pub file: Option<Vec<String>>,
    #[serde(
        default,
        rename = "depends_on",
        deserialize_with = "deserialize_dependencies"
    )]
    pub dependencies: BTreeMap<String, DependencyKind>,
    #[serde(skip)]
    pub dependants: BTreeSet<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

impl Stack {
    pub fn directory(&self, base: &Path) -> PathBuf {
        if let Some(ref dir) = self.directory {
            base.join(dir)
        } else {
            base.join(&self.key)
        }
    }
}

impl PartialEq for Stack {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

fn default_true() -> bool {
    true
}

fn default_command() -> Vec<String> {
    vec!["docker".to_string(), "compose".to_string()]
}

/// Describes the dependency cycle formed by `path` followed by `dep`.
fn cycle_error(stacks: &BTreeMap<String, Stack>, path: &[String], dep: &str) -> String {
    let start = path.iter().position(|key| key == dep).unwrap();
    let cycle: Vec<&String> = path[start..].iter().chain(path.get(start)).collect();

    let links: Vec<String> = cycle
        .windows(2)
        .map(|pair| {
            format!(
                "\"{}\" depends on \"{}\" ({})",
                pair[0], pair[1], stacks[pair[0]].dependencies[pair[1]]
            )
        })
        .collect();

    format!("invalid dependency cycle: {}", links.join(", "))
}

fn check_dependencies(
    stack: &Stack,
    stacks: &BTreeMap<String, Stack>,
    path: &mut Vec<String>,
) -> Result<(), String> {
    path.push(stack.key.clone());

    for dep in stack.dependencies.keys() {
        if path.contains(dep) {
            return Err(cycle_error(stacks, path, dep));
        }

        if let Some(inner) = stacks.get(dep) {
            check_dependencies(inner, stacks, path)?;
        } else {
            return Err(format!(
                "invalid dependency: \"{}\" is not a known stack",
                dep
            ));
        }
    }

    path.pop();

    Ok(())
}

fn deserialize_stacks<'de, D>(deserializer: D) -> Result<BTreeMap<String, Stack>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut stacks: BTreeMap<String, Stack> = BTreeMap::deserialize(deserializer)?;

    for (key, stack) in stacks.iter_mut() {
        stack.key = key.clone();
        if stack.name.is_empty() {
            stack.name = key.clone();
        }
    }

    link_stacks(&mut stacks).map_err(D::Error::custom)?;

    Ok(stacks)
}

/// Validates the dependencies of every stack and fills in their dependants.
fn link_stacks(stacks: &mut BTreeMap<String, Stack>) -> Result<(), String> {
    for stack in stacks.values_mut() {
        stack.dependants.clear();
    }

    let keys: Vec<String> = stacks.keys().cloned().collect();
    let mut path = Vec::new();
    for key in keys {
        let stack = stacks.get(&key).unwrap();
        check_dependencies(stack, stacks, &mut path)?;

        let dependencies: Vec<String> = stack.dependencies.keys().cloned().collect();
        for dep in dependencies {
            let stack = stacks.get_mut(&dep).unwrap();
            stack.dependants.insert(key.clone());
        }
    }

    Ok(())
}

#[serde_as]
#[derive(Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub base_dir: PathBuf,
    #[serde(default = "default_command")]
    #[serde_as(as = "StringWithSeparator::<SpaceSeparator, String>")]
    pub command: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_stacks")]
    pub stacks: BTreeMap<String, Stack>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub infer_dependencies: bool,
}

fn add_dependencies(stacks: &BTreeMap<String, Stack>, stack: &str, keys: &mut BTreeSet<String>) {
    let stack = stacks.get(stack).unwrap();

    for dep in stack.dependencies.keys() {
        if keys.contains(dep) {
            continue;
        }
        keys.insert(dep.to_owned());

        add_dependencies(stacks, dep, keys);
    }
}

fn add_dependants(stacks: &BTreeMap<String, Stack>, stack: &str, keys: &mut BTreeSet<String>) {
    let stack = stacks.get(stack).unwrap();

    for dep in stack.dependants.iter() {
        if keys.contains(dep) {
            continue;
        }
        keys.insert(dep.to_owned());

        add_dependants(stacks, dep, keys);
    }
}

impl Config {
    fn stack_keys<I, S>(&self, list: I) -> Result<BTreeSet<String>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Use an ordered set here so results are stable.
        let mut keys: BTreeSet<String> = BTreeSet::new();

        for key in list {
            let key_str = key.as_ref();

            if self.stacks.contains_key(key_str) {
                keys.insert(key_str.to_owned());
            } else {
                return Err(format!("unknown stack \"{}\"", key_str));
            }
        }

        // An empty initial list means we want all stacks.
        if keys.is_empty() {
            Ok(self.stacks.keys().cloned().collect())
        } else {
            Ok(keys)
        }
    }

    fn stacks_from_known_keys<I>(&self, keys: I) -> Vec<&Stack>
    where
        I: IntoIterator<Item = String>,
    {
        keys.into_iter()
            .map(|k| self.stacks.get(&k).unwrap())
            .collect()
    }

    pub fn from_reader<R: Read>(base_dir: &Path, reader: R) -> Result<Self, String> {
        let mut config: Config = serde_yaml_ng::from_reader(reader).map_err(|e| e.to_string())?;
        config.base_dir = base_dir.to_owned();
        Ok(config)
    }

    /// Adds dependencies inferred from the docker compose configuration of
    /// each stack, if enabled.
    pub fn add_inferred_dependencies(&mut self) -> Result<(), String> {
        if !self.infer_dependencies {
            return Ok(());
        }

        let inferred = infer::infer_config_dependencies(self);
        self.add_dependencies(inferred)
    }

    fn add_dependencies(
        &mut self,
        inferred: BTreeMap<String, BTreeSet<String>>,
    ) -> Result<(), String> {
        for (key, deps) in inferred {
            for dep in deps {
                // A dependency that is both explicit and inferred stays explicit.
                let stack = self.stacks.get_mut(&key).unwrap();
                if let Entry::Vacant(entry) = stack.dependencies.entry(dep) {
                    log::debug!("Inferred dependency of \"{}\" on \"{}\"", key, entry.key());
                    entry.insert(DependencyKind::Inferred);
                }
            }
        }

        link_stacks(&mut self.stacks)
    }

    pub fn stacks_with_dependencies<I, S>(&self, list: I) -> Result<Vec<&Stack>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // First list all the desired stacks. Use an ordered set here so results
        // are stable.
        let mut keys = self.stack_keys(list)?;

        // Add all dependencies if needed.
        if keys.len() < self.stacks.len() {
            for key in keys.clone().iter() {
                add_dependencies(&self.stacks, key, &mut keys);
            }
        }

        // Now to order them in dependent order.
        let mut added: HashSet<String> = HashSet::new();
        let mut stacks = Vec::new();

        while stacks.len() != keys.len() {
            for key in keys.iter().rev() {
                if added.contains(key) {
                    // Already added this one.
                    continue;
                }

                let stack = self.stacks.get(key).unwrap();
                if stack
                    .dependencies
                    .keys()
                    .all(|dep: &String| added.contains(dep) || !keys.contains(dep))
                {
                    // All dependencies we care about are resolved so this stack
                    // can be added.
                    stacks.push(stack);
                    added.insert(key.to_owned());
                }
            }
        }

        Ok(stacks)
    }

    pub fn stacks_with_dependants<I, S>(&self, list: I) -> Result<Vec<&Stack>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // First list all the desired stacks. Use an ordered set here so results
        // are stable.
        let mut keys = self.stack_keys(list)?;

        // Add all dependants if needed.
        if keys.len() < self.stacks.len() {
            for key in keys.clone().iter() {
                add_dependants(&self.stacks, key, &mut keys);
            }
        }

        // Now to order them in dependant order.
        let mut added: HashSet<String> = HashSet::new();
        let mut stacks = Vec::new();

        while stacks.len() != keys.len() {
            for key in keys.iter().rev() {
                if added.contains(key) {
                    // Already added this one.
                    continue;
                }

                let stack = self.stacks.get(key).unwrap();
                if stack
                    .dependants
                    .iter()
                    .all(|dep: &String| added.contains(dep) || !keys.contains(dep))
                {
                    // All dependants we care about are resolved so this stack
                    // can be added.
                    stacks.insert(0, stack);
                    added.insert(key.to_owned());
                }
            }
        }

        Ok(stacks)
    }

    pub fn stacks<I, S>(&self, list: I) -> Result<Vec<&Stack>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(self.stacks_from_known_keys(self.stack_keys(list)?))
    }

    pub fn stack<I, S>(&self, list: I) -> Result<Vec<&Stack>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let stacks = self.stacks(list)?;
        if stacks.len() != 1 {
            Err(format!(
                "Only one stack can be used but {} were provided.",
                stacks.len()
            ))
        } else {
            Ok(stacks)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Stack};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn from_str(s: &str) -> Result<Config, String> {
        Config::from_reader(&PathBuf::default(), s.as_bytes())
    }

    fn keys(stacks: Vec<&Stack>) -> Vec<String> {
        stacks.iter().map(|s| s.key.clone()).collect()
    }

    #[test]
    fn name() {
        let config = from_str(
            "
            stacks:
                foo:
                    name: baz
                bar: {}
            ",
        )
        .unwrap();

        let stacks = config.stacks(["foo", "bar"]).unwrap();
        assert_eq!(stacks.len(), 2);
        let stack = stacks.first().unwrap();
        assert_eq!(stack.name, "bar");
        let stack = stacks.get(1).unwrap();
        assert_eq!(stack.name, "baz");
    }

    #[test]
    fn stacks() {
        let config = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - bar
                bar:
                    depends_on:
                        - baz
                baz: {}
            ",
        )
        .unwrap();
        let list: [&str; 0] = [];
        let stacks = keys(config.stacks(list).unwrap());
        assert_eq!(
            stacks,
            vec!["bar".to_string(), "baz".to_string(), "foo".to_string()]
        );

        let stacks = keys(config.stacks(["bar"]).unwrap());
        assert_eq!(stacks, vec!["bar".to_string()]);

        let stacks = keys(config.stacks_with_dependencies(["baz"]).unwrap());
        assert_eq!(stacks, vec!["baz".to_string()]);
        let stacks = keys(config.stacks_with_dependants(["baz"]).unwrap());
        assert_eq!(
            stacks,
            vec!["baz".to_string(), "bar".to_string(), "foo".to_string()]
        );

        let stacks = keys(config.stacks_with_dependencies(["bar"]).unwrap());
        assert_eq!(stacks, vec!["baz".to_string(), "bar".to_string()]);
        let stacks = keys(config.stacks_with_dependants(["bar"]).unwrap());
        assert_eq!(stacks, vec!["bar".to_string(), "foo".to_string()]);

        let stacks = keys(config.stacks_with_dependencies(["foo"]).unwrap());
        assert_eq!(
            stacks,
            vec!["baz".to_string(), "bar".to_string(), "foo".to_string()]
        );
        let stacks = keys(config.stacks_with_dependants(["foo"]).unwrap());
        assert_eq!(stacks, vec!["foo".to_string()]);

        assert_eq!(
            config.stacks(["bar", "biz"]).err().unwrap(),
            "unknown stack \"biz\""
        );

        let error = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - baz
            ",
        )
        .err()
        .unwrap();
        assert_eq!(
            &error,
            "invalid dependency: \"baz\" is not a known stack at line 2 column 13"
        );

        let error = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - baz
                baz:
                    depends_on:
                        - foo
            ",
        )
        .err()
        .unwrap();
        assert_eq!(
            &error,
            "invalid dependency cycle: \"baz\" depends on \"foo\" (explicit), \"foo\" depends on \"baz\" (explicit) at line 2 column 13"
        );

        let error = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - bar
                baz:
                    depends_on:
                        - foo
                bar:
                    depends_on:
                        - baz
            ",
        )
        .err()
        .unwrap();
        assert_eq!(
            &error,
            "invalid dependency cycle: \"bar\" depends on \"baz\" (explicit), \"baz\" depends on \"foo\" (explicit), \"foo\" depends on \"bar\" (explicit) at line 2 column 13"
        );

        let config = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - bar
                bar:
                    depends_on:
                        - baz
                        - biz
                biz:
                    depends_on:
                        - baz
                baz: {}
            ",
        )
        .unwrap();
        let stacks = keys(config.stacks_with_dependencies(["foo"]).unwrap());
        assert_eq!(
            stacks,
            vec![
                "baz".to_string(),
                "biz".to_string(),
                "bar".to_string(),
                "foo".to_string()
            ]
        );

        let stacks = keys(config.stacks(["bar"]).unwrap());
        assert_eq!(stacks, vec!["bar".to_string()]);
    }

    fn inferred(list: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
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
    fn inferred_dependencies() {
        let mut config = from_str(
            "
            stacks:
                foo:
                    depends_on:
                        - bar
                bar: {}
                baz: {}
            ",
        )
        .unwrap();
        assert!(config.infer_dependencies);

        config
            .add_dependencies(inferred(&[("foo", &["bar"]), ("bar", &["baz"])]))
            .unwrap();

        let stacks = keys(config.stacks_with_dependencies(["foo"]).unwrap());
        assert_eq!(
            stacks,
            vec!["baz".to_string(), "bar".to_string(), "foo".to_string()]
        );
        let stacks = keys(config.stacks_with_dependants(["baz"]).unwrap());
        assert_eq!(
            stacks,
            vec!["baz".to_string(), "bar".to_string(), "foo".to_string()]
        );

        // Inferred dependencies that would create a cycle are an error. The
        // error shows that "foo" on "bar" stays explicit despite being inferred.
        assert_eq!(
            config
                .add_dependencies(inferred(&[("baz", &["foo"])]))
                .err()
                .unwrap(),
            "invalid dependency cycle: \"bar\" depends on \"baz\" (inferred), \"baz\" depends on \"foo\" (inferred), \"foo\" depends on \"bar\" (explicit)"
        );

        let config = from_str(
            "
            infer_dependencies: false
            stacks:
                foo: {}
            ",
        )
        .unwrap();
        assert!(!config.infer_dependencies);
    }
}
