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

The first SIGINT or SIGTERM stops source schedules, dashboard serving, and idle
queue waits. Active triage can finish before the process exits. A second signal
forces exit status 130.

## Dashboard only

The explicit dashboard command serves the same read-only interface without
running the monitor or owning the triage queue:

```sh
intake dashboard --host 127.0.0.1 --port 4545
```
