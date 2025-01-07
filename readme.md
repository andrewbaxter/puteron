# What is Puteron

Puteron is a process manager, like SystemD or Runit or Shepherd or any number of others.

Here's a quick comparison to SystemD:

- Represents tasks (services) as a graph (like SystemD)

- Separate user control state (what you instructed it to do) from operational state (what circumstances forced)

- Easy to reason about one-directional dependencies

- JSON everywhere: configs, command output, command input

- Does one thing: manage processes

# Using it, in a nutshell

Build `puteron` with `cargo build` (or get it some other way).

1. Define task (service/job) config and put them in a directory - see the task JSON specification below

2. Define the `puteron` config and specify the above task directory - see the config JSON specification below

3. Run `puteron demon run config.json`

# Architecture

## Control and actual state

Tasks are either on or off (control state), and started or stopped (actual state). The control state is further divided into two parts: `user_on` and `transitive_on`.

### Control state

- `user_on` is whether the user requested the task to run (i.e. by setting it to default on or by running `task on`).

- `transitive_on` is whether this task needs to be started for another task with `user_on` to run (more details in dependencies).

These are logically-unioned to produce a single `on` value. To put it another way, a task is `on` if 1. the user says they need it on or 2. if any other task the user says they need on (transitively) needs it.

If a task is `on` Puteron will try to make sure it's running (start it, restart it if it failed, etc). If a task is `off` Puteron will try to make sure it's not running (stop it, force kill it if that fails).

You can see tasks which affect the `transitive_on` value of a task with `puteron task list-downstream`.

### Actual state

- `started` - it depends on the task type, but as an example, for perpetual tasks, the task is considered started only when

  - the control state is `on`

  - the process is running

  - any startup check has passed

  A change in any of the above will cause the task to transition to/from `started`

- `stopped` - when the task isn't `started`

These two states are significant for dependency calculations/graph management.

Under the hood though there's a more fine grained state, with a multi-step startup and stop process.

## Starting and stopping tasks

When a task becomes `on`, any `strong` upstream dependencies of the task will be started (per `transitive_on` behavior). The task itself won't be started until every dependency has started. You can see which tasks affect startup using `puteron task upstream`.

When a task becomes `off`, once any dependents have stopped, the task will be stopped (if a processes, signalled), and once the process finishes for this task it will repeat for any dependencies that have also become `off`.

Task dependencies are specified one-directionally in the depender (for analogy to SystemD, there's `Wants` but no `WantedBy`).

## Demon config JSON specification

- [Demon config JSON Schema](TODO)

## Task JSON specification

All other operational details are in the task JSON schema

- [Task JSON Schema](TODO)

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

## API

All control is done via unix-domain stream sockets with 64-bit LE length-framed JSON messages.

- [API message JSON Schema](TODO)

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
