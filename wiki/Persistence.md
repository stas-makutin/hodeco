# Persistence

hodeco stores message history and task history on disk. Neither requires an
external database.

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

## Task history

History is opt-in. Each task with history enabled owns a history file under
`tasks/`; the task's `history.maxSize` and `history.maxRows` control its limits.
The task editor starts at 200 rows / 100 KiB when history is enabled.

Stored runs include trigger metadata and payload, executed actions, selector
result, started next tasks, chain information, and the error. Only executed
actions are stored. Run rows use a monotonically increasing `time` for paging.

History is file-backed. The service keeps per-task row/byte metadata and the
latest summary, not the full run set, and reads the file on demand. The separate
in-memory run feed is volatile and is not part of persistent task history.

`tasks.purgeHistory` clears the task's history file, removes that task's feed
rows, resets the in-memory `maxRuns` counters, and clears the cached last-run
summary. Deleting a task removes its history file. Files for tasks that no
longer exist are removed at startup.

## Crash behavior

- Message records may lose up to one flush interval (default 60 s).
- Task history flushes each completed run.
- A partial trailing write is discarded; the valid prefix remains readable.
- Compaction preserves the newest rows within the configured limits.
