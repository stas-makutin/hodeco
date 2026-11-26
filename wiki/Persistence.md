# Persistence

hodeco writes three different kinds of history to disk. None of them needs an
external database. The first two are covered here; the operational log is covered
in [Logging](Logging). Start with the table and read the section that matches
your question.

| History | File | What it answers | Survives restart? |
| --- | --- | --- | --- |
| Message history | `messages.log` | What did the devices say? | Yes (up to the last flush). |
| Task history | `tasks/<task-id>.tlog` | What did my automation do? | Yes, when enabled per task. |
| Operational log | `hodeco.log` | What did the gateway do? | Yes, subject to rotation. |

## Message history

Default: `messages.log`, 10 MiB, flush interval 60 s.

The message history records incoming and outgoing messages, state changes, and
evictions from the bounded in-memory store. When the store exceeds its size
limit, the oldest messages are removed.

The file is compacted when it grows above `maxSize × compactionFactor` (default
`2.0`) or after `compactInterval` (default 30 days). Compaction rewrites the
live messages to a temporary file and replaces the original atomically.

`messageLog.onCorrupt` controls unsupported or corrupt files:

- `ignore` replaces the file at startup;
- `fail` refuses startup.

Archive or delete an old message log before upgrading from 0.1.x.

### If you only remember one thing

`messages.log` is the file to keep if you want message history after an upgrade
or a move. Deleting it starts the gateway with an empty history.

## Task history

History is opt-in. Each task with history enabled owns a history file under
`tasks/`; the task's `history.maxSize` and `history.maxRows` control its limits.
The task editor starts at 200 rows / 100 KiB when history is enabled.

Stored runs include trigger metadata and payload, executed actions, selector
result, started next tasks, chain information, and the error. Only executed
actions are stored. Run rows use a monotonically increasing `time` for paging.

History is file-backed. The service keeps per-task row/byte metadata and the
latest summary, not the full run set, and reads the file on demand. The separate
in-memory run feed is volatile and is not part of persistent task history. See
[Tasks](Tasks) for the feed/history split and how to enable history.

`tasks.purgeHistory` clears the task's history file, removes that task's feed
rows, resets the in-memory `maxRuns` counters, and clears the cached last-run
summary. Deleting a task removes its history file. Files for tasks that no
longer exist are removed at startup.

## Backups, upgrades, and resets

- **Back up** the five YAML files, `messages.log`, and the `tasks/` directory if
  you want to preserve both configuration and history.
- **Upgrade** from 0.1.x: archive or delete the old message log first; the file
  format changed.
- **Reset message history**: stop hodeco, delete `messages.log`, and start again.
- **Reset one task's history**: use `tasks.purgeHistory` rather than deleting a
  file while the gateway is running.
- **Move to another machine**: keep `config.yaml` and the four catalogs, and
  update any absolute paths. Keep `messages.log` and `tasks/` if you want the
  old history.

The exact file paths are in the [Configuration](Configuration) defaults table.

## Crash behavior

- Message records may lose up to one flush interval (default 60 s).
- Task history flushes each completed run.
- A partial trailing write is discarded; the valid prefix remains readable.
- Compaction preserves the newest rows within the configured limits.

## Related pages

- [Logging](Logging) — the operational log and rotation.
- [Configuration](Configuration) — `messageLog` and `taskLog` settings.
- [Tasks](Tasks) — enabling per-task history and reading runs.
- [Releases](Releases) — upgrades and package upgrades.
