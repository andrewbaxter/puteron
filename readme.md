# What is Puteron

Puteron is a process manager, like SystemD or Runit or Shepherd or any number of others.

Here's a quick comparison to SystemD:

- Represents tasks (services) as a graph (like SystemD)

- One-directional dependencies

- Your intentions are inviolable - see `Control and actual state` below

- JSON everywhere: configs, command output, command input

- Minimal external dependencies, so it's easy to test configs with a local demon as a user

- Does one thing: manage processes

# Architecture

## Control and actual state

Tasks are either on or off (the "control state"), and started or stopped (or starting/stopping... this is the "actual state").

The control state represents your intentions or the desired state of the system, and the actual state is the result of Puteron trying to establish that state while obeying dependency rules, dealing with failures, etc.

### Control state

The control state is further divided into two parts: `user_on` and `transitive_on`.

- `user_on` is whether the user requested the task to run directly (i.e. by directly setting it to `default_on` or by running `task on`).

- `transitive_on` is whether this task needs to be started for another task with `user_on` to run.

These are logically-unioned to produce a single `on` value. To put it another way, a task is `on` if 1. the user says they need it on or 2. if any other task the user says they need on (transitively) needs it.

If a task is `on` Puteron will try to make sure it and all of its dependencies are running (start it, restart it if it failed, etc). If a task is `off` Puteron will try to make sure it's not running (stop it, force kill it if that fails) and stop any no-longer required dependencies.

You can see tasks which affect the `transitive_on` state of a task with `puteron task list-downstream`.

### Actual state

- `started` - it depends on the task type, but as an example, for perpetual tasks, the task is considered started only when

  - the process is running

  - any startup check has passed

  A change in any of the above will cause the task to transition to/from `started`

- `stopped` - when the task isn't `started`

These two states are significant for dependency calculations/graph management.

Under the hood though there's a more fine grained state (`starting`, `stopping`) but only the two above states affect whether dependencies will be started or stopped.

### Starting and stopping tasks

When a task becomes `on`, any `strong` upstream dependencies of the task will be started (per `transitive_on` behavior), the task itself wil be started, then any downstream dependencies that are `on` will be started.

When a task becomes `off`, all downstream dependencies are stopped, the task itself is stopped, then upstream dependencies are stopped. once any dependents have stopped, the task will be stopped (if a processes, signalled), and once the process finishes for this task it will repeat for any dependencies that have also become `off`.

Both of these processes respect dependencies: before any task is started all upstream dependencies must be started and before any task is stopped, any downstream dependencies must be stopped. Tasks will wait for these conditions to be true before state changes are initiated.

# Using it, in a nutshell

Build `puteron` with `cargo build` (or get it some other way).

1. Write config for your tasks put them in a directory - [reference](#demon-config)

2. Define the `puteron` config and specify the above task directory - [reference](#task-specification)

3. Run `puteron demon run config.json`

## Nix

Nix definitions are provided in

- `source/package.nix` - just the derivation to build the binary

- `source/module.nix` - Nice, type-checked configs via `config.puteron = { ... }`. Sets `puteron` up as a SystemD unit.

  It closely follows the JSON schema, so refer to that for config details.

You can use it by doing

```nix
{
  modules = [ ./path/to/puteron/source/module.nix ];
  config = {
    puteron = {
      ...
    };
  };
}
```

## Reference

### Demon config

- [Demon config JSON Schema](./source/generated/jsonschema/config.schema.json)

An example config: `config.json`

```json
{
  "environment": {
    "keep": {
      "XDG_RUNTIME_DIR": true
    }
  },
  "task_dirs": ["/path/to/my/tasks/dir"]
}
```

I kept the `XDG_RUNTIME_DIR` env var since that's used to determine the path for `puteron` IPC for running `puteron` commands in tasks (see the backup task below). This allows me to use the same config as root (home server) or as a user (local testing).

### Task specification

- [Task JSON Schema](./source/generated/jsonschema/task.schema.json)

An example task: `sunwet.json`

```json
{
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

An exampled scheduled task: `backup_b2.json`

```json
{
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
puteron task off sunwet-backup-lock
puteron task wait-until-stopped sunwet
rclone --config /path/to/config sync /my/data default_remote:/my-bucket/local
puteron task on sunwet-backup-lock
```

### API

All control is done via unix-domain stream sockets with 64-bit LE length-framed JSON messages.

- [API request JSON Schema](./source/generated/jsonschema/api_request.schema.json)
- [API response JSON Schemas](./source/generated/jsonschema)

# Unnecessary details

## Origin story

So I should have taken better notes, but IIRC I had a server that primarily ran a single service, and I wanted that service always up.

The behavior was pretty straight forward: I wanted the service to start at boot, I wanted dependencies to be automatically started, I wanted it to start _after_ its dependencies, and if a dependency failed (e.g. network disk went away temporarily) the chain of dependencies should be stopped until the disk came back, in which they would be restarted again in dependency-order. And if I stopped the service, any unused dependencies should be stopped too.

AFAICT this was not possible in SystemD... (at time of writing)

- `Requires` - dependencies would be started, but if one failed the service wouldn't be stopped
- `BindsTo` - dependencies would be started, and if one failed the service would be stopped, but this would overwrite my desired "start" control and if the dependencies came back it wouldn't be re-started
- `PartOf` - doesn't start dependencies
- `Upholds` - constantly restarts services that are down, filling the logs with spam and potentially causing other issues
- Other combinations of the above, inappropriate use of other parameters, etc

These option names rely on (IMO) made up distinctions between near-synonyms to convey their intent. Just figuring out the differences between those took a good few hours, and I guarantee that I can't 100% predict their behavior still ([I'm not alone](https://pychao.com/2021/02/24/difference-between-partof-and-bindsto-in-a-systemd-unit/)). And more keep on getting added, when some hole is noticed in between the other definitions.

Then there's things like: if there's a `.device` in your dependency path (implicit in mount units!), and it gets removed (e.g. disk removed for a mount) [it'll cause a "stop" not a "failure"](https://github.com/systemd/systemd/issues/30204) so everything will stop and not be retried. This is similar to `ConditionZZZ` directives, which force a service into a stopped state rather than just delaying startup.

There are a million directives with complex, subtle interactions, and there were a number of other things that kept biting me like no/limited JSON support making interop difficult, it was hard to figure out why processes were getting killed or restarted, commands would exit with code 0 even when invocations were wrong, commands would return non-0 codes when things were fine, etc.

I believe there's a simpler model for process management.

## What's needed for a process manager

To even have a chance of getting users, I needed to figure out what the core features SystemD has or roles it fills that any replacement would need. I'm not sure I have the answer, but after asking around and doing some research here's a list of my best guess:

- A standard for packaging

  Many pieces of software come with reference SystemD unit definitions. This was huge for interoperability, and unfortunately this is purely a market share thing - I'm hopeful this will be surmountable if Puteron is otherwise very usable.

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

I think Puteron has solutions for most of these, to some degree or minimal wrapper scripts. You can't do direct conversion of SystemD units to Puteron tasks, but I think most services could be run in Puteron without much hassle.

I believe a significant part of SystemD's design and features were also there to make migration easier (like unit generators from `/etc` files and init.d scripts) and simultaneously support everything those could possibly do. This isn't a goal for Puteron.
