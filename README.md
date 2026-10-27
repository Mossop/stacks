# stacks

[![Tests](https://github.com/Mossop/stacks/actions/workflows/test.yml/badge.svg)](https://github.com/Mossop/stacks/actions/workflows/test.yml)

Stacks is a tool for managing docker compose projects. It supports running
commands against multiple projects and allows defining environment variables for
each in order to provide a configuration of sorts.

Stacks also supports the notion of cross-project dependencies. In an ideal world
each project is a self-contained unit not depending on any other project. In
reality projects can reference and depend on networks and containers defined in
other projects. Stacks lets you define those dependencies such that when
attempting to bring up one project it ensures that the other projects that it
requires are up. Many of these dependencies can also be inferred automatically
(see [Inferred dependencies](#inferred-dependencies)).

## Commands

The general format for commands is:
```
stacks <options> <stacks> [command] <args>
```
* `stacks` is a comma separated list of stacks to apply the command to. If not
present then the command will be applied to all stacks.
* `command` is the command to run. All docker compose commands are supported
with some alterations and additions as listed below.
* `args` are additional arguments to pass through to docker compose.

For each stack listed on the command line and all of their dependencies docker
compose will be run to perform the command. The commands will be run in the
appropriate order.

The following commands are either additional on top of those provided by docker
compose or slightly modified versions of those in docker compose:
* `stack <stacks> up <args>`: Brings up the given stacks. Equivalent to calling
`docker compose up --wait` for all the required stacks.
* `stack <stacks> update <args>`: Pulls new images and recreates any required
stacks. Equivalent to calling `docker compose pull` and then
`docker compose up --wait` for all the required stacks.

## Configuration

Stacks must be configured with a simple yaml file, by default it walks the
working directory and its parents looking for a `stacks.yml` file though that
can be overridden with the `-f` argument or the `STACKS_FILE` environment
variable.

```yaml
stacks:
  networks:
    file: networks.yml
  web:
    depends_on:
      - networks
```

A few global properties can be set:

* `command`: The path used to invoke docker compose. Defaults to
`docker compose` but in some cases you may want to set this to `docker-compose`
or provide an absolute path in case docker is not in the `PATH`.
* `infer_dependencies`: Whether to infer dependencies between stacks from their
compose configuration. Defaults to `true`.

The key for each stack in the configuration file is its default name and acts as
the default project directory. The following properties may be set for each
stack:

* `name`: The name to use for the project in docker, defaults to the key in the
config file.
* `directory`: The path to the compose project relative to the stacks config,
defaults to the key in the config file.
* `file`: The path (or list of paths) to the compose project relative to the
stack config, defaults the same logic that docker compose uses.
* `depends_on`: A list of the stacks that this stack depends on.
* `environment`: a dictionary to define environment variables. This allows a
certain amount of customisation of the stack.

## Inferred dependencies

In addition to those listed in `depends_on`, stacks infers dependencies by
running `docker compose config` for every stack. A stack depends on another
stack when it references something that the other stack creates:

* An external network (`external: true`) whose name matches a network defined
by the other stack.
* An external volume whose name matches a volume defined by the other stack.
* A container, via `network_mode: container:<name>`,
`volumes_from: container:<name>` or `external_links`, whose name matches one
of the other stack's containers (either its `container_name` or the default
`<project>-<service>-1`).

Inference only happens for commands where ordering matters (`up`, `down`,
`restart`, etc.). Stacks whose configuration cannot be loaded are skipped with a
warning. A dependency cycle, whether inferred or explicit, is an error. Note
that docker compose omits networks and volumes that no service uses, so those
can't be inferred. Set `infer_dependencies: false` to disable inference entirely.
