# Intagent

**Your agent for incoming work.**

Intagent is my experimental system for turning incoming emails into local work. It watches
configured sources, runs a restricted triage agent for each event, and decides whether I need to
act.

It's built around my workflow where I use [aven] to manage tasks and [workmux] to run coding agents
in isolated worktrees and tmux windows. This makes the default setup fairly specific to me, but the
pieces are replaceable. Sources communicate through a small JSON protocol, and routing behavior
lives in agent skills. Supporting another mail provider, task manager, or agent runner mostly means
providing a source command and adapting those skills.

## How it works

Email is the main intake layer. Direct messages, automated alerts, and GitHub notifications all
arrive as events that the triage agent can evaluate.

When something requires action, the agent creates or updates an aven task. If the message warrants
investigation, it starts a workmux agent in the relevant project and places it in that project's
tmux session. I can watch the investigation, inspect its worktree, and continue the agent
conversation when it needs more context or judgment.

Informational messages are recorded without creating unnecessary work. Incoming content is treated
as untrusted data, and outward actions such as sending email, posting comments, pushing code, or
merging changes remain under my control.

## Why investigations run locally

The decision to run investigations in tmux on my laptop is deliberate. I don't care about having
agents running in the cloud. It's perfectly fine for them to pick things up when I open my laptop.

This puts the investigation where I want it: in the project's tmux session, alongside everything
else I'm doing. I can watch it, inspect the worktree, and continue the agent conversation when
necessary.

## Usage

Run continuously:

```sh
intagent watch
```

Run the monitor and local dashboard together:

```sh
intagent watch --dashboard
```

Poll every source once and process the resulting queue:

```sh
intagent check
```

See [the CLI guide](docs/cli.md) and [example configuration](examples/config.yaml) for the rest of
the setup and command surface.

[aven]: https://github.com/raine/aven
[workmux]: https://github.com/raine/workmux
