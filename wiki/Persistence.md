# Persistence

hodeco keeps two append-only logs. Neither requires an external database.

## Shared container

Both logs use the same framing:

```text
magic (4) | version (u8) | reserved (3) | compacted_at (i64 ns)
record_type (u8) | record_length (u32) | record_body
```

Records are written until EOF. A partial trailing record is ignored on load and
truncated before the next append. Middle corruption follows the file's
`onCorrupt` policy. Compaction writes a temporary file, fsyncs it, and renames it
atomically. The compaction threshold is `maxSize * compactionFactor` (default
`2.0`) or `compactInterval` (default 30 days).

## Message log — HLOG v2

Default: `messages.log`, 10 MiB, flush interval 60 s.

Records:

- `MESSAGE_NEW`: protocol, transport, entry, message ID, time, state, payload.
- `MESSAGE_STATE`: message ID and new state.
- `MESSAGE_DROPPED`: message IDs removed from the bounded store.

The service key is repeated in each NEW record; there is no service table.
Compaction writes one NEW record per live message.

**HLOG v1 is not supported.** There is no migration, rename, or dual reader.
With `messageLog.onCorrupt: ignore` a v1 file is replaced at startup; with
`fail` startup is refused. Archive or delete the old message log before
upgrading from 0.1.x.

## Task history — TLOG

Each task owns `tasks/<id>.tlog`, default 100 KiB / 1,000 rows. Records contain
typed blocks rather than JSON:

- run core, chain;
- typed trigger blocks (message, schedule, HTTP, manual, task);
- executed command and message actions;
- selector result, started next tasks, error.

Only executed actions are stored. A run row has a global monotonically
increasing `time` for paging. Task history flushes immediately. Orphan `.tlog`
files whose task no longer exists are removed at startup.

`tasks.purgeHistory` rewrites the file with an empty header and resets the
in-memory `maxRuns` counters. Deleting a task removes its history file.
`maxRuns` is no longer derived from TLOG rows, so trimming history does not
affect it.

## Crash behavior

- Message records may lose up to one flush interval (default 60 s).
- Task history flushes each completed run.
- A partial trailing record is discarded; a valid prefix remains readable.
- Compaction preserves the newest rows within the configured limits.
