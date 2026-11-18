# Changelog

All notable changes to hodeco are recorded here. The project follows semantic
versioning for the application release. The JSON-RPC protocol version is listed
separately because it can stay compatible across releases.

## 0.2.0 — 2026-11-17

Phase 2 adds a protocol-agnostic automation engine, reusable schedules and
commands, per-task run history, and the Tasks area of the UI.

### Breaking changes

- **Configuration is split into five files.** `config.yaml` no longer accepts a
  `services` list. Services, tasks, schedules, and commands live in
  `services.yaml`, `tasks.yaml`, `schedules.yaml`, and `commands.yaml` next to
  the core configuration. There is no migration from the old single-file shape;
  an old `services` key now fails startup with an unknown-field error.
- **The message log format is incompatible.** Message history now uses the
  HLOG v2 append log. Existing v1 files are not migrated, renamed, or read.
  With `messageLog.onCorrupt: ignore` (the default) a v1 file is replaced at
  startup; with `onCorrupt: fail` startup is refused. Delete or archive the old
  message log before upgrading.
- **The HTTP task action is removed.** Use a command such as `curl` or
  PowerShell for webhooks and HTTP requests.
- **Task actions always accept extra arguments.** The `allowTaskCommand` gate
  was removed. Command definitions are a reuse mechanism, not a security
  boundary.
- **One active run per task.** There is no overlap, queue, or per-task
  concurrency setting. Triggers that arrive while a task is running are
  suppressed and counted; they are not replayed later.
- **Task and schedule state is not caught up after downtime.** Missed schedule
  occurrences are skipped.

### Dependencies

- Added `chrono-tz` and `iana-time-zone` for IANA timezone and system-zone
  handling. Both are required for correct DST, calendar, and solar scheduling.

### Configuration

- Added `files` to point at the four sibling catalogs.
- Added `taskLog` for the per-task append log (`maxSize`, `maxRows`,
  `compactionFactor`, `compactInterval`, `onCorrupt`).
- Added `taskEngine` limits (`maxTriggerPayloadSize`, `maxConcurrentRuns`,
  `maxChainDepth`, `maxFanOut`).
- Added `messageLog.compactionFactor` and `messageLog.compactInterval`.
- `messageLog.autoPersist` now defaults to 60 seconds and is the append-log
  flush interval.

### Automation

- Tasks with message, schedule, HTTP, manual/API, and task triggers.
- Sequential command and message actions with `failFast`, `runTimeout`, and
  optional 8 KiB-per-run output capture.
- Next-task rules: `always`, `allSucceeded`, `anyFailed`, `allFailed`, and
  `command-result` with exit codes `0..254` (`255` is reserved for selector
  errors).
- Reusable command catalog with JSON on stdin, fixed plus task arguments, an
  optional working directory, and a timeout.
- Reusable schedule catalog with fixed times, hourly minute marks, civil
  twilight and solar events, calendar intervals, day/month/year filters,
  allow/deny windows, exceptions, loops, and deterministic jitter.
- Sunrise and sunset apply the observer elevation horizon-dip correction.
- HTTP hook at `/api/hooks/{task}` with optional `X-Hodeco-Secret`.
- Per-task append-only history with summaries, full run details, paging, purge,
  compaction, and orphan-file cleanup at startup. `history.maxRows: 0` disables
  history for that task. Schedule-trigger `maxRuns` is counted in memory and is
  independent of history.
- `tasks.*`, `schedules.*`, and `commands.*` RPC methods, plus
  `events.taskRunStarted` and `events.taskRunFinished` on the `tasks` topic.

### UI

- Top-level navigation is now Services, Messages, Tasks, Configuration, Server,
  and Console; Services contains Services and Discovery sub-tabs.
- Tasks contains Tasks, Schedules, Commands, and Runs.
- The task list mirrors the Services table, with text row actions and stable
  empty/filter states.
- Full-page task and schedule editors, a command modal, and a Runs view with
  inline details.

### Protocol

- Protocol version stays `1.0`. The new methods are additive at the transport
  level; the configuration shape change is an application/configuration break.

## 0.1.0

Initial public release.
