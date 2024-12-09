# What is this

This is a process manager, like SystemD or runit or any number of others.

Here's a quick comparison to SystemD:

- Like SystemD, represents services as a graph

- Different, hopefully simpler behavior model

- JSON everywhere: configs, command output, command input

- Does one thing: manage processes

# Nutshell

1. Define tasks (services, processes, jobs) as JSON

2. Either put them in an autoload directory or call `puterium task create task.json` once puterium is running

3. Run puterium with `puterium demon run`

# Graph logic

## Control and actual state

Tasks are either on or off (control state), and started or stopped (actual state). The control state is further divided into two parts: `user_on` and `transitive_on`.

### Control state

- `user_on` is whether the user requested the task to run (i.e. by setting it to default on or by running `task on`).

- `transitive_on` is whether this task needs to be started for another task with `user_on` to run (more details in dependencies).

These are logically-unioned to produce `on`. To put it another way, a task is `on` if 1. the user says they need it on or 2. if any other task the user says they need on (transitively) needs it.

If a task is `on` puterium will try to make sure it's running (start it, restart it if it failed, etc). If a task is `off` puterium will try to make sure it's not running (stop it, force kill it if that fails).

You can visualize this for a task with `puterium task why-on`.

### Actual state

- `started` - it depends on the task type, but as an example, for perpetual tasks, the task is considered started only when

  - the control state is `on`

  - the process is running

  - any startup check has passed

  A change in any of the above will cause the task to transition to/from `started`

- `stopped` - when the task isn't `started`

These two states are significant for dependency calculations/graph management.

Under the hood though there's a more fine grained state, with a multi-step startup and stop process.

### Non-existant and deleted tasks

Non-existant and deleted tasks are considered `off` and `stopped` for dependency calculations.

## Starting and stopping tasks

When a task becomes `on`, any `hard` dependencies will be started (per `transitive_on` behavior). The task itself won't be started until every dependency has started.

When a task becomes `off`, once any dependents have stopped, the task will be stopped (if a processes, signalled), and once the process finishes for this task it will repeat for any dependencies that have also become `off`.

Task dependencies are specified one-directionally in the depender (for analogy to SystemD, `Wants` but no `WantedBy`).

## API

All control is done via unix-domain datagram sockets with versioned JSON messages.

- [API message JSON Schema](TODO)

## Everything else (reference)

All other operational details are in the task JSON schema

- [Task JSON Schema](TODO)

# Unnecessary details

## Origin story

So I should have taken better notes, but IIRC I had a server that primarily ran a single service, and I wanted that service always up.

The behavior was pretty simple: I wanted the service to start at boot, I wanted dependencies to be automatically started, I wanted it to start _after_ its dependencies, and if a dependency failed (e.g. network disk went away temporarily) the chain of dependencies would be stopped until the disk came back, in which they would be restarted again in dependency-order.

AFAICT this was not possible in SystemD:

- `requires`

And the whole thing was super complex with tons of options with unclear interactions or behaviors spread out in various places. The operation model doesn't match the simple mental model I have at all, and there were lots of other things I didn't like about SystemD as well: no/limited JSON support making interop difficult, it was hard to figure out why processes were getting killed or restarted, commands would exit with code 0 even when invocations were wrong, commands would return non-0 codes when things were fine, etc.
