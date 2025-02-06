# Invariants

- No cycles.

- All upstreams always exist, and therefore all downstreams also always exist.

- Short tasks with non-none started actions cannot have downstreams

# Status flow, long task

```mermaid
flowchart TD
    stopped --> starting
    starting --> started
    starting --> stopping
    started --> stopping
    stopping --> stopped
```

# Status flow, short task

```mermaid
flowchart TD
    stopped --> starting
    starting --> started
    starting -->|error| starting
    started --> stopped
```
