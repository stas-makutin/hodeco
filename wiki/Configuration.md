# Configuration

hodeco reads a core `config.yaml` plus four sibling catalogs:
`services.yaml`, `tasks.yaml`, `schedules.yaml`, and `commands.yaml`.
With no files at all it starts with sensible defaults, so the smallest useful
configuration is just the serial service you want to talk to:

```yaml
# services.yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0   # COM4 on Windows
    params:
      baudRate: 115200
```

Everything else can be left implicit until you need it. Unknown fields are
rejected, so typos fail at startup instead of being silently ignored. There is no
migration from the pre-0.2.0 shape where `services` lived in `config.yaml`.

The platform defaults are:

| Item | Windows | Linux |
| --- | --- | --- |
| Config | `<exe_dir>\config.yaml` | `/etc/hodeco/config.yaml` |
| Working directory | `<exe_dir>` | `/var/lib/hodeco` |
| Log file | `<exe_dir>\log\hodeco.log` | `/var/log/hodeco/hodeco.log` |
| Message log | `<exe_dir>\messages.log` | `/var/lib/hodeco/messages.log` |
| Task history | `<working_dir>\tasks` | `/var/lib/hodeco/tasks` |
| Static assets | `<exe_dir>\site` | `/usr/share/hodeco/site` |

Relative core paths are resolved after the configured `workingDir` is created
and entered. Catalog paths in `files` are resolved against the directory that
contains `config.yaml`.

## Full reference

```yaml
# config.yaml
workingDir: /var/lib/hodeco

log:
  dir: /var/log/hodeco
  file: hodeco.log
  dirMode: "0755"
  fileMode: "0644"
  maxSize: 100 MiB
  maxAge: 1 d
  backups: 5
  backupDays: 14
  archive: none       # none or zip

httpServer:
  bind: 127.0.0.1
  port: 8180
  maxConnections: 1000
  readTimeout: 5 s
  readHeaderTimeout: 5 s
  writeTimeout: 5 s
  idleTimeout: 2 m
  maxHeaderBytes: 8192
  static:
    dir: /usr/share/hodeco/site
    index: index.html
    spaFallback: true
    precompressed: true

messageLog:
  maxSize: 10 MiB
  file: /var/lib/hodeco/messages.log
  fileMode: "0644"
  autoPersist: 60 s      # append-log flush interval
  compactionFactor: 2.0
  compactInterval: 30 d
  onCorrupt: ignore      # ignore or fail

files:
  services: services.yaml
  tasks: tasks.yaml
  schedules: schedules.yaml
  commands: commands.yaml

taskLog:
  dir: tasks
  maxSize: 100 KiB
  maxRows: 1000
  fileMode: "0640"
  compactionFactor: 2.0
  compactInterval: 30 d
  onCorrupt: ignore

taskEngine:
  maxTriggerPayloadSize: 16 KiB   # hard maximum 64 KiB
  maxConcurrentRuns: 32
  maxChainDepth: 16
  maxFanOut: 16
```

```yaml
# services.yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0
    params:
      baudRate: 115200
      openAttemptsInterval: 3 s
      outgoingMaxTTL: 10 s
```

```yaml
# commands.yaml
commands:
  - id: meter
    name: Meter poll
    executable: /usr/local/bin/meter-poll
    args: ["--json"]
    workingDir: /var/lib/hodeco   # optional
    timeout: 10s
    description: Read the meter value
```

```yaml
# schedules.yaml
location: [52.52, 13.405, 34]   # lat, lon, elevation m
timezone: Europe/Berlin

schedules:
  - id: before-sunset
    name: Before sunset
    on:
      at: "sunset - 15m"
      days: [mon, tue, wed, thu, fri]
    loop:
      every: 30m
      repeat: 2
```

```yaml
# tasks.yaml
tasks:
  - id: evening-lights
    name: Evening lights
    enabled: true
    failFast: true
    runTimeout: 5m
    maxRunsPerMinute: 60
    history:
      maxSize: 100 KiB
      maxRows: 1000
    triggers:
      - id: sunset
        type: schedule
        schedule: before-sunset
        maxRuns: 365
    actions:
      - id: meter
        command: meter
        args: ["--once"]
        output: capture
      - id: notify
        message:
          service: { alias: ZW }
          payload: { source: trigger }
          ttl: 30s
    next:
      - when: allSucceeded
        tasks: log-success
```

See the [task](Tasks) and [schedule](Schedules) pages for trigger, action,
next-rule, calendar, solar, and loop details.

## Notes

- Service aliases and keys, task/schedule/command IDs, and trigger/action IDs
  must be unique.
- Task, schedule, and command IDs are immutable ASCII slugs
  (`[a-z][a-z0-9_-]{0,63}`). The server generates one when an API/UI create
  request omits it.
- `outgoingMaxTTL` bounds how long an outgoing message may wait in the service
  queue. The default is `10 s`; `0 s` disables the TTL. The message builder can
  override it per send without changing the service configuration.
- `openAttemptsInterval` controls how often a service retries opening its
  transport; the default is `3 s`.
- `services.*`, `tasks.*`, `schedules.*`, and `commands.*` RPC methods rewrite
  the relevant file atomically. The service account must be able to write those
  files for write-back to succeed.
- Removing a service or changing its alias is rejected while a task trigger or
  message action references it. Remove the task references first.
- Config changes made outside the service require a manual restart; there is no
  file watcher. Missing catalog files are treated as empty.
- `log.maxSize: 0` or `log.maxAge: 0 s` disables that rotation trigger.
  Rotation is enabled only when retention is configured.
- `messageLog.onCorrupt` is `ignore` or `fail`. The message log is HLOG v2;
  v1 files are not migrated. With `ignore`, a v1 or corrupt file is replaced at
  startup. With `fail`, startup is refused.
- `taskLog.onCorrupt` applies to individual `tasks/<id>.tlog` files. Orphan
  `.tlog` files are removed at startup.
- `httpServer.static.precompressed` enables serving `.br`/`.gz` sidecars and
  defaults to `true`. Release builds ship raw assets plus `.br` and `.gz`
  sidecars; the raw file remains the fallback for clients that accept neither.
  JSON definition files are always served uncompressed so runtime edits are
  visible. For other assets, regenerate the sidecars when editing raw files; a
  stale sidecar can shadow a newer raw file.

See [Logging](Logging) and [HTTP API](HTTP-API) for the runtime behavior behind
these settings.
