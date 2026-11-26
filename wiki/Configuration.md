# Configuration

hodeco reads a core `config.yaml` plus four sibling catalogs:
`services.yaml`, `tasks.yaml`, `schedules.yaml`, and `commands.yaml`.

You do not have to create all five. With no files at all hodeco starts with
sensible defaults, and the smallest useful setup is a single serial service:

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

Everything else can stay implicit until you need it. This page starts with the
files and paths, then walks through the core sections, the catalogs, and the
behavior that is easy to miss.

> **New to the terms?** Read [How hodeco works](Concepts) first. It explains
> what a service, message, task, and schedule are.

## The five files

| File | Purpose | Usually edited |
| --- | --- | --- |
| `config.yaml` | Core settings: server, logging, message history, paths, task-engine limits. | Rarely, after the first run. |
| `services.yaml` | One entry per controller. | When adding or moving a controller. |
| `commands.yaml` | External executables that tasks can run. | When adding automation. |
| `schedules.yaml` | Shared timezone/location and reusable schedules. | When adding time-based automation. |
| `tasks.yaml` | Triggers, actions, next rules, and history settings. | When adding automation. |

The catalogs can also be managed from the UI and the corresponding RPC methods.
`config.yaml` is the only file updated through `config.update`.

## Where the files live

The configuration file is optional. These are the platform defaults:

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
contains `config.yaml`, not against the working directory. This makes it safe to
keep the catalogs next to `config.yaml` even when the working directory is
somewhere else.

Missing catalog files are treated as empty. Unknown fields are rejected, so a
typo fails at startup instead of being silently ignored. There is no migration
from the pre-0.2.0 shape where `services` lived in `config.yaml`.

## Core configuration

This is a complete `config.yaml` with the defaults and the fields most people
change:

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
  archive: zip        # zip (default) or none

httpServer:
  bind: 127.0.0.1
  port: 8180
  maxConnections: 1000  # 0 removes the practical limit
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
  autoPersist: 60 s      # how often message history is written to disk
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
  fileMode: "0640"
  compactionFactor: 2.0
  compactInterval: 30 d
  onCorrupt: ignore

taskEngine:
  maxTriggerPayloadSize: 16 KiB   # hard maximum 64 KiB
  maxConcurrentRuns: 32
  maxChainDepth: 16
  maxFanOut: 16
  runFeed:
    maxRows: 1000                 # recent runs kept in memory
    maxSize: 4 MiB                # byte cap; oldest run evicted first
```

### Paths and `workingDir`

`workingDir` is where relative core paths start. hodeco creates the directory if
needed and enters it before resolving relative paths. On Windows the default is
the executable directory; on Linux it is `/var/lib/hodeco` for a system install.

If you move data somewhere else, prefer absolute paths for `log.dir`,
`messageLog.file`, `taskLog.dir`, and `httpServer.static.dir`. Keeping everything
relative to a portable `config.yaml` works well for a release archive; see
[Getting Started](Getting-Started).

### Operational log (`log`)

| Field | What it controls |
| --- | --- |
| `dir`, `file` | Where `hodeco.log` is written. |
| `dirMode`, `fileMode` | Unix permission bits for the directory and file. |
| `maxSize` | Rotate when the file reaches this size. |
| `maxAge` | Rotate when the file reaches this age. |
| `backups` | Rotated files kept per day when `backupDays` is set; otherwise the total number of rotated files to keep. |
| `backupDays` | Number of days of rotated history to keep. |
| `archive` | `zip` (default) compresses rotated files; `none` keeps them plain. |

`maxSize: 0` or `maxAge: 0 s` disables that rotation trigger. Rotation is
enabled only when retention is configured as well. The full behavior is covered
in [Logging](Logging).

### HTTP server and the UI (`httpServer`)

| Field | What it controls |
| --- | --- |
| `bind`, `port` | The listening address. The default is `127.0.0.1:8180`. |
| `maxConnections` | Connection cap; `0` removes the practical limit. |
| `readTimeout`, `readHeaderTimeout`, `writeTimeout`, `idleTimeout` | Request and connection timeouts. |
| `maxHeaderBytes` | Maximum request header size. |
| `static.dir` | Directory containing the built UI (`site/` or `hodeco-ui/dist`). |
| `static.index` | Index file name, normally `index.html`. |
| `static.spaFallback` | Serve the index for extensionless navigation paths. |
| `static.precompressed` | Serve `.br`/`.gz` sidecars when the client accepts them. |

The default bind address is loopback only. There is no authentication or TLS; if
you need access from other machines, put hodeco behind a reverse proxy that
provides both.

`precompressed: true` serves a matching `.br` or `.gz` sidecar and falls back to
the raw file when the client accepts neither. JSON definition files are always
read from the raw file so runtime edits are visible; for other assets, regenerate
the sidecars after editing a raw file, because a stale sidecar can shadow a
newer raw file.

### Message history (`messageLog`)

| Field | What it controls |
| --- | --- |
| `maxSize` | Size limit of the in-memory message store. |
| `file` | The persisted history file. |
| `fileMode` | Unix permission bits for the file. |
| `autoPersist` | How often the store is flushed to disk. |
| `compactionFactor` | Compact when the file grows above `maxSize × factor`. |
| `compactInterval` | Compact after this much time, even below the size trigger. |
| `onCorrupt` | `ignore` replaces an unsupported/corrupt file at startup; `fail` refuses startup. |

The message log is the file to keep if you want history after an upgrade or a
move. Deleting it starts the gateway with empty message history. See
[Persistence](Persistence) for the details and crash behavior.

### Catalog paths (`files`)

```yaml
files:
  services: services.yaml
  tasks: tasks.yaml
  schedules: schedules.yaml
  commands: commands.yaml
```

These paths are relative to the directory containing `config.yaml`. You can
point them at a different directory as long as the hodeco service can read and
write them.

### Task history (`taskLog`)

| Field | What it controls |
| --- | --- |
| `dir` | Directory for per-task history files. |
| `fileMode` | Unix permission bits for history files. |
| `compactionFactor`, `compactInterval` | When history files are compacted. |
| `onCorrupt` | `ignore` or `fail`, applied to each task's history file. |

Files for tasks that no longer exist are removed at startup. History is opt-in
per task; see [Tasks](Tasks) for `history.maxRows` and `history.maxSize`.

### Task engine (`taskEngine`)

| Field | What it controls |
| --- | --- |
| `maxTriggerPayloadSize` | Maximum trigger payload; the hard ceiling is 64 KiB. |
| `maxConcurrentRuns` | Global cap on active task runs. |
| `maxChainDepth` | Maximum depth of a next-rule chain. |
| `maxFanOut` | Maximum number of tasks started from one run. |
| `runFeed.maxRows`, `runFeed.maxSize` | Bounds of the volatile recent-runs buffer. |

The run feed is independent of per-task history. It clears on restart and holds
the most recent completed runs from every task. See [Tasks](Tasks) for the
feed/history split.

## Service catalog (`services.yaml`)

```yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0
    params:
      baudRate: 115200
      openAttemptsInterval: 3 s
      outgoingMaxTTL: 10 s
  - alias: ZB
    protocol: zigbee
    transport: serial
    entry: /dev/ttyUSB1
    params:
      baudRate: 57600
      flowControl: none
      openAttemptsInterval: 3 s
      outgoingMaxTTL: 10 s
      ashAckTimeout: 1.6 s
      ashMaxRetries: 5
```

| Field | Meaning |
| --- | --- |
| `alias` | Short name used by the UI, API, and task references. Keep it stable. |
| `protocol` | `zwave` or `zigbee`. |
| `transport` | `serial` today. |
| `entry` | Serial port. Use a stable by-id path on Linux when possible. |
| `params` | Protocol and transport parameters. The exact set is exposed by `protocols.params`. |

The parameters that usually matter:

- `baudRate` — the controller's serial speed. The examples above are common
  defaults, not universal; check the controller's manual.
- `openAttemptsInterval` — how often hodeco retries opening the port. Default
  `3 s`.
- `outgoingMaxTTL` — how long an outgoing message may wait in the service queue
  before it expires. Default `10 s`; `0 s` disables the TTL. The message
  builder's **Override TTL** changes it for one send without touching the
  service configuration.
- `readTimeout` — bounds the wait for a Z-Wave serial reply; `0 s` uses the
  1.5 s Z-Wave default. ZigBee uses its ASH timers instead.
- `writeTimeout` — bounds serial writes; `0 s` disables the timeout.
- `flowControl` — `none`, `software`, or `hardware`. The HUSBZB-1 ZigBee side
  works with `none` at 57600 8N1; use `software` if your stick or firmware
  requires XON/XOFF.
- `ashAckTimeout`, `ashMaxRetries` — the ZigBee ASH link layer. The gateway owns
  ACK/NAK handling; clients send and receive EZSP frames. ASH
  ACK/NAK/RST/RSTACK/ERROR frames are stored as raw link messages in the message
  history.

Service aliases and keys must be unique. You can manage services from the
**Services** view or the `services.*` methods; edits to the file itself require a
restart.

## Command catalog (`commands.yaml`)

Commands are external executables that tasks can run.

```yaml
commands:
  - id: meter
    name: Meter poll
    executable: /usr/local/bin/meter-poll
    args: ["--json"]
    workingDir: /var/lib/hodeco   # optional
    timeout: 10s
    description: Read the meter value
```

| Field | Meaning |
| --- | --- |
| `id` | Immutable ASCII slug, unique in the catalog. |
| `name` | Display name. |
| `executable` | Program to run. |
| `args` | Fixed arguments; task arguments are appended after these. |
| `workingDir` | Optional working directory. |
| `timeout` | Optional per-command timeout. |
| `description` | Optional note shown in the UI. |

Commands run as the hodeco service account. See [Tasks](Tasks) for stdin,
stdout capture, and what happens on failure.

## Schedule catalog (`schedules.yaml`)

Schedules define *when*; tasks reference them.

```yaml
location: [52.52, 13.405, 34]   # lat, lon, elevation m
timezone: Europe/Berlin

schedules:
  - id: before-sunset
    name: Before sunset
    at: "sunset - 15m"
    days: [mon, tue, wed, thu, fri]
    repeatAt: [30m, 1h]
```

The `location` and `timezone` keys are shared defaults. A schedule can override
either. See [Schedules](Schedules) for fixed times, solar events, recurrence,
repeats, filters, and previews.

## Task catalog (`tasks.yaml`)

A task combines triggers, actions, and optional next rules.

```yaml
tasks:
  - id: evening-lights
    name: Evening lights
    labels: [lighting, automated]
    enabled: true
    runTimeout: 10m
    triggers:
      - id: sunset
        type: schedule
        schedule: before-sunset
    actions:
      - id: meter
        command: meter
        args: ["--once"]
        output: capture
      - id: notify
        message:
          service: { alias: ZW }
          payload: "AQID"
          ttl: 30s
    next:
      - when: allSucceeded
        tasks: log-success
```

The task page covers trigger types, action behavior, next rules, run limits, and
history in detail: [Tasks](Tasks).

## How changes take effect

There are two different paths:

| Change | When it applies |
| --- | --- |
| `config.update` (UI or API) | The core config is validated and written atomically. The response lists sections that need a restart; hodeco does not restart itself. |
| `services.*`, `tasks.*`, `schedules.*`, `commands.*` | The relevant catalog is written atomically and applied immediately. No restart is needed. |
| Editing a YAML file directly | Requires a restart. There is no file watcher. |

The service account must be able to write the config and catalog files for
write-back to succeed. The packages make `/etc/hodeco` group-writable by
`hodeco`; a manual install may need a writable `--config` path or adjusted
permissions.

Removing a service or changing its alias is rejected while a task references
it. Deleting a schedule or command that a task still uses is rejected with
`scheduleInUse` or `commandInUse`; the error includes the task IDs. Disabled
tasks still count as users. Remove the references first, then delete the catalog
entry.

## Validation and IDs

- Service aliases and keys, task/schedule/command IDs, and trigger/action IDs
  must be unique.
- Task, schedule, and command IDs are immutable ASCII slugs matching
  `[a-z][a-z0-9_-]{0,63}`. The server generates one when an API/UI create
  request omits it.
- Unknown fields are rejected.
- Missing catalog files are treated as empty; a missing `config.yaml` means the
  platform defaults.
- A task's optional `delay` and `jitter` are limited to 24 hours combined, and
  `delay + jitter` must be less than `runTimeout`.
- Task labels are optional. A task can have up to 32 labels, each up to 32
  characters. Labels are trimmed and must be unique within the task,
  case-insensitively.

## Troubleshooting configuration

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| Startup fails with an unknown field | A typo or a field from an older version. | Remove or correct the field. Unknown fields are rejected on purpose. |
| Duplicate ID or alias error | Two entries share a key. | Make IDs and aliases unique within their catalog. |
| Changes in the UI do not persist | The service account cannot write the file. | Fix ownership/permissions, or use a writable `--config` path. |
| A deleted service keeps coming back in errors | A task still references its alias. | Update or remove the task references first. |
| A duration or delay is rejected | The value is outside the allowed range, or `delay + jitter` is not less than `runTimeout`. | Reduce the delay/jitter and keep the combined value under 24 hours and below `runTimeout`. |
| The UI still uses old files after an edit | The file was edited directly. | Restart the gateway; there is no file watcher. |

## Related pages

- [How hodeco works](Concepts) — the vocabulary used above.
- [Getting Started](Getting-Started) — first run and service setup.
- [Tasks](Tasks) and [Schedules](Schedules) — the catalogs in practice.
- [Logging](Logging) and [Persistence](Persistence) — the runtime files.
- [HTTP API](HTTP-API) — `config.get`, `config.update`, and the catalog methods.
