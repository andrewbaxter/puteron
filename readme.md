# What is Puteron

Puteron is a process manager, like Systemd, Runit, Shepherd, etc..

Here's a quick comparison to systemd:

- Represents task (service) dependencies as a graph (like systemd)

- Set your desired state and Puteron does what's required to establish exactly what you want and keep it that way. Intent and execution are separated - see `Control and actual state` below

- One-directional dependencies: no "wanted by", "part of", "binds to", etc

- Easy startup checks! Wait for a file to appear or a socket to open before starting downstream tasks. Dbus notification isn't supported.

- JSON everywhere: configs, command output, command input

- Minimal external dependencies (dbus, hardcoded paths, polkit, logind, etc.) so it's easy to run multiple instances for labs or development, or test a system config as a user

- Does one thing: manage processes

# Architecture

Puteron manages a dependency graph of "tasks". If task `B` depends on `A` we call `A` the upstream of `B`, and `B` the downstream of `A`. An upstream dependency tells Puteron that `A` can't run unless `B` has started, and if `B` stops then `A` must be stopped.

There are two types of dependencies:

- `strong` dependencies - turning a task on will try to start the upstream dependency too. For example, `my-app` should start `mysql`.

- `weak` dependencies - the task won't start until the dependency turns on. For example `my-app-monitor` can be turned on, but won't start until `my-app` is also started.

## Control and actual state

The above "turning a task on" is the "control" state. Control state is `on` or `off`.

The "actual state" is the result of Puteron trying to establish the control state. Actual state is `stopped`, `starting`, `started`, `stopping`.

### Control state

Internally, the control state is divided into three parts: `direct_on`, `transitive_on`, and `effective_on`.

- `direct_on` is whether what you, the user, specify (i.e. by configuring a task as `default_on` or by running `task on`).

- `transitive_on` is established when a task, connected by one or more _strong_ _downstream_ dependency links, is turned on.

- `effective_on` or just `on` colloquially - `effective_on` is what Puteron uses to decide whether to start or stop a task. This is `(direct_on || transitive_on) && all_weak_upstream_effective_on` (if it's not clear, the 3rd is a value that's true when all `weak` upstream dependencies have `effective_on`).

If a task is "on" Puteron will try to make sure it and all of its strong dependencies are running (start it, restart it if it failed, etc). If a task is "off" (not "on") Puteron will try to make sure it's not running (stop it, force kill it if that fails) and stop any no-longer required dependencies.

This also means that if a task is not directly on, and the task(s) making it `transitive_on` are turned off, then that task will also be stopped. Puteron maintains a minimal running state.

You can see the tasks which are enabling `transitive_on` of a task with `puteron list-downstream --transitive-on`.

### Actual state

Changes to "actual" state trigger Puteron to propagate control state - i.e. when the "actual" state of a task reaches `started` the downstream dependencies can be started according to `on`, etc.

- `stopped` - this is the initial state of all tasks. Upstream dependencies won't stop until all of their downstreams have reached `stopped`.

- `starting`, `stopping` - these are transitional states. `short` tasks are `starting` until the command completes, while `long` tasks are `starting` until their `started_check` passes (or immediately, if they have no started check).

- `started` - this is when the task is fully operational. Downstream dependencies won't start until all of their upstreams have reached `started`.

# Using it, in a nutshell

Build `puteron` with `cargo build` (or get it some other way).

1. Write config for your tasks and put them in a directory - [reference](#task-specification)

2. Define the `puteron` config `config.json` and specify the above task directory - [reference](#demon-config)

3. Run `puteron demon config.json`

4. You can use `puteron` to turn tasks on or off, load new tasks, or delete existing tasks. See the command help for details.

## Nix

Nix definitions are provided in

- `source/package.nix` - just the derivation to build the binary

- `source/module.nix` - Sets up `puteron` as a systemd unit and allows configuration via `config.puteron`, plus shortcuts for systemd interop (listening and control via puteron tasks).

You can use it by doing

```nix
{
  modules = [ ./path/to/puteron/source/module.nix ];
  config = {
    puteron = {
      tasks.my_task = { type = "long"; ... };
    };
  };
}
```

Each entry in `config.puteron.tasks.*` is directly translated to JSON so you must use the same format for that, including capitalization.

## Reference

### Demon config

- [Demon config JSON Schema](./source/generated/jsonschema/config.schema.json)

An example config: `config.json`

```json
{
  "$schema": "https://raw.githubusercontent.com/andrewbaxter/puteron/refs/heads/master/source/generated/jsonschema/config.schema.json",
  "environment": {
    "keep": {
      "XDG_RUNTIME_DIR": true
    }
  },
  "task_dirs": ["/path/to/my/tasks/dir"]
}
```

(Specifying the schema is optional but will make VS Code provide autocomplete and check the config as you write it.)

I kept the `XDG_RUNTIME_DIR` env var since that's used to determine the path for `puteron` IPC for running `puteron` commands recursively in tasks if you happen to want to do that (see the backup task below - this allows me to use the same config as root (home server) or as a user (local testing)).

### Task specification

- [Task JSON Schema](./source/generated/jsonschema/task.schema.json)

An example task: `sunwet.json`

```json
{
  "$schema": "https://raw.githubusercontent.com/andrewbaxter/puteron/refs/heads/master/source/generated/jsonschema/task.schema.json",
  "type": "long",
  "upstream": {
    "fdap-oidc": "strong",
    "openfdap": "strong",
    "spaghettinuum": "strong",
    "sunwet-backup-lock": "weak"
  },
  "default_on": true,
  "command": {
    "command": ["${bin}/bin/sunwet", "run-server", "/path/to/my/config"]
  },
  "started_check": {
    "tcp_socket": "127.0.0.1:4567"
  }
}
```

(Specifying the schema is optional but will make VS Code provide autocomplete and check the config as you write it, if you're writing JSON directly)

An exampled scheduled task: `backup_b2.json`

```json
{
  "type": "short",
  "command": {
    "command": ["/path/to/backup/script"]
  },
  "schedule": [
    {
      "weekly": {
        "weekday": "monday",
        "time": "5:00"
      }
    }
  ]
}
```

with backup script:

```bash
set -xeu
rclone --config /path/to/config sync /my/data default_remote:/my-bucket/local
```

### Task types

- `long`

  This is a typical daemon/service/server. You'd use it for things like Postgres or a web server that run continuously.

- `short`

  This is for something that runs and is expected to stop on its own. Things like backup scripts, batch jobs, etc. Short tasks can be set to run periodically on a schedule. Short tasks can turn their control state off after running, or delete themselves.

- `empty`

  When `on` this immediately reaches `started` state, and when off it immediately reaches `stopped` state. It does nothing else.

  You can use this to group tasks (i.e. having tasks as strong upstreams, then turning on the empty will turn on all the tasks) or to proxy state from something managed externally (i.e. have an external system update the state of the empty so other tasks can respond to it, see the next section for one use case).

### Interaction with systemd

There are two utilities/hacks to work with systemd:

- Binary `puteron-control-systemd`

  This binary attempts to manage the state of a systemd unit. When run, it starts the systemd unit. When killed, it stops the systemd unit. If the systemd unit exits on its own, `puteron-control-systemd` also exits.

  You can use this to allow the puteron graph to control systemd units, by making a task that runs `puteron-control-systemd`. When the task is started or stopped, the corresponding systemd unit will also be started or stopped.

- You can add `ExecStartPost` and `ExecStopPre` commands to hook `puteron on` and `puteron off` with a proxy `empty` unit, to allow systemd to control the puteron graph.

There's options to do these automatically in the provided nix module.

### API

All control is done via unix-domain stream sockets with 64-bit LE length-framed JSON messages.

[IPC JSON Schemas](./source/generated/jsonschema/)

# Unnecessary details

## Origin story

So I should have taken better notes, but IIRC I had a server that primarily ran a single service, and I wanted that service always up.

The behavior was pretty straight forward: I wanted the service to start at boot, I wanted dependencies to be automatically started, I wanted it to start _after_ its dependencies, and if a dependency failed (e.g. network disk went away temporarily) the chain of dependencies should be stopped until the disk came back, in which they would be restarted again in dependency-order. And if I stopped the service, any unused dependencies should be stopped too.

AFAICT this was not possible in systemd... (at time of writing)

- `Requires` - dependencies would be started, but if one failed the service wouldn't be stopped
- `BindsTo` - dependencies would be started, and if one failed the service would be stopped, but this would overwrite my desired "start" control and if the dependencies came back it wouldn't be re-started
- `PartOf` - doesn't start dependencies
- `Upholds` - constantly restarts services that are down, filling the logs with spam and potentially causing other issues
- Other combinations of the above, inappropriate use of other parameters, etc

These option names rely on (IMO) made up distinctions between near-synonyms to convey their intent. Just figuring out the differences between those took a good few hours, and I guarantee that I can't 100% predict their behavior still ([I'm not alone](https://pychao.com/2021/02/24/difference-between-partof-and-bindsto-in-a-systemd-unit/)). They form a matrix of behaviors with gaps, and more keep on getting added when some new gap is noticed in between the other definitions.

Then there's things like: if there's a `.device` in your dependency path (implicit in mount units!), and it gets removed (e.g. disk removed for a mount) [it'll cause a "stop" not a "failure"](https://github.com/systemd/systemd/issues/30204) so everything will stop and not be retried. This is similar to `ConditionZZZ` directives, which force a service into a stopped state rather than just delaying startup.

There are a million directives with complex, subtle interactions, and there were a number of other things that kept biting me like no/limited JSON support making interop difficult, it was hard to figure out why processes were getting killed or restarted, commands would exit with code 0 even when invocations were wrong, commands would return non-0 codes when things were fine, etc.

I believe there's a simpler model for process management.

## What's needed for a process manager

To even have a chance of getting mindshare, I needed to figure out what the core features systemd has or roles it fills that any replacement would need. I'm not sure I have the answer, but after asking around and doing some research here's a list of my best guess:

- A standard for packaging

  Many pieces of software come with reference systemd unit definitions. This was huge for interoperability, and unfortunately this is purely a market share thing - I'm hopeful this will be surmountable if Puteron is otherwise very usable.

- A standard for overrides

  System administrators need a standard method for customizing services and manage those overrides independent from the base system so that upgrades don't fail to update files or overwrite customizations.

- Avoiding misconfiguration when there are optional dependencies

  If a service self-configures itself based on the observed environment (like, open sockets, or external processes, enumerating devices, etc) then if it's started too early it may be mis-configured. Specifying dependencies prevents such a service from starting until the environment is properly set up.

- Dealing with diverse installation environments

  Depending on the system, files may be in different locations, services might not exist (or others might exist in their place). Ideally, a single definition would be able to work without changes in a variety of environments, various runtime checks, etc.

- System mutation

  For maintenance or on various external events it may be necessary to stop or start sets of services.

- Log management

  All processes should be logged - a process manager should never discard output unless explicitly instructed to.

- Reducing log noise

  The dependency graph prevents services from starting when critical dependencies aren't on, avoiding a lot of error spam in the noise.

I think Puteron has solutions for most of these, to some degree or minimal wrapper scripts. You can't do direct conversion of systemd units to Puteron tasks, but I think most services could be run in Puteron without much hassle.

I believe a significant part of systemd's design and features were also there to make migration easier (like unit generators from `/etc` files and init.d scripts) and simultaneously support everything those could possibly do. This isn't a goal for Puteron.
