# Intake CLI

## Continuous monitoring

Run the monitor until it receives SIGINT or SIGTERM:

```sh
intake watch
```

Add `--dashboard` to serve the read-only monitoring dashboard from the same
process and database:

```sh
intake watch --dashboard
```

The dashboard listens on `127.0.0.1:4545` by default. Its bind options follow
the flag:

```sh
intake watch --dashboard --host 127.0.0.1 --port 8080
```

A non-loopback host exposes an unauthenticated title and entity API. It requires
an explicit acknowledgement:

```sh
intake watch --dashboard --host 0.0.0.0 --allow-non-loopback
```

`--config` remains a global option and works before or after the command:

```sh
intake --config ~/intake/config.yaml watch --dashboard
intake watch --dashboard --config ~/intake/config.yaml
```

## Skills

`intake init` creates a private skill directory beside the generated
configuration, normally `~/.config/intake/skills`. Intake discovers only skills
listed in `skills.directories`; installed examples are templates and are not
active by default.

The default installer places templates under:

```text
~/.local/share/doc/intake/examples/skills
```

Copy a template into the private skill directory and customize its task manager
or investigation workflow as needed. The included routing templates demonstrate
Aven as the task manager while allowing a different task manager to be
substituted. Add commands required by the skill to `commands.rules` in the
configuration.

The first SIGINT or SIGTERM stops source schedules, dashboard serving, and idle
queue waits. Active triage can finish before the process exits. A second signal
forces exit status 130.

## Dashboard only

The explicit dashboard command serves the same read-only interface without
running the monitor or owning the triage queue:

```sh
intake dashboard --host 127.0.0.1 --port 4545
```
