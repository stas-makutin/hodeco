# Logging

hodeco writes two different things, for two different questions:

| File | Question it answers |
| --- | --- |
| `hodeco.log` | What did the service do? Startup, configuration, HTTP, serial, and error events. |
| `messages.log` | What did the devices say? The persisted frame history that powers the message views. |

They have separate retention and rotation settings because they grow at very
different rates. If you are troubleshooting, start with the table below.

## Which log should I look at?

| Symptom | Start with |
| --- | --- |
| The process will not start | `hodeco.log`, then stdout/stderr or the Event Log if the log file was never opened. |
| The HTTP server is not listening | `hodeco.log` for the bind address and bind errors. |
| A controller will not connect | `hodeco.log` for the serial open error, then check the port and permissions. |
| Frames are missing or decode oddly | The Messages view first, then `messages.log` for persistence issues. |
| A task did not run or failed | The Runs view or `tasks.getRun`; `hodeco.log` may show spawn or configuration errors. |
| History disappeared after a restart | [Persistence](Persistence) — message history and task history have different rules. |

## Operational log

Operational logs go to the configured rotating file. The `log` section controls
size and age limits, backup retention, and ZIP archiving:

```yaml
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
```

- `maxSize: 0` or `maxAge: 0 s` disables that rotation trigger.
- Rotation is enabled only when retention is configured as well.
- `archive: zip` compresses rotated files and is the default; set `archive: none`
  to keep plain rotated files.

Log records are queued to a dedicated writer thread, so file I/O, rotation, and
archiving never block Tokio request or serial tasks. The queue is bounded: if
the writer cannot keep up, new records are dropped rather than stalling the
gateway. Message-log appends, flushes, compaction, initial loading, task-history
reads/writes, and configuration write-back run on the blocking pool too.

File log lines use a single-letter level and one space before the message:

```text
2026-01-01 12:00:00.000 I http server listening
2026-01-01 12:00:01.000 W service retry scheduled
```

Levels are `E` (error), `W` (warning), `I` (info), `D` (debug), and `T` (trace).

stdout/stderr and the Windows Event Log are reserved for initialization and
fatal process failures. Linux does not use logrotate or stdout redirection for
the application log.

## Message log

The message log persists the in-memory message history across restarts:

```yaml
messageLog:
  maxSize: 10 MiB
  file: /var/lib/hodeco/messages.log
  fileMode: "0644"
  autoPersist: 60 s
  onCorrupt: ignore  # ignore or fail
```

- `maxSize` bounds the in-memory message store.
- `autoPersist` controls periodic persistence; the store is also saved on
  shutdown.
- `onCorrupt` decides whether a corrupt snapshot is ignored or treated as a
  startup failure.

The message file is the one to keep if you want history after an upgrade or a
move. Deleting it starts the gateway with an empty history. See
[Persistence](Persistence) for compaction, retention, and crash behavior.

## Service logs

When running as a Windows service, operational logs go to the configured log
file and the Event Log is used for startup/configuration failures and log-writer
errors.

When running under systemd, initialization and fatal messages go to
stdout/stderr, which systemd forwards to the journal; operational events still go
to the application log file.

```sh
journalctl -u hodeco
```

The systemd unit uses `LogsDirectory=hodeco` and `StateDirectory=hodeco`, so
`/var/log/hodeco` and `/var/lib/hodeco` are created with the service user's
ownership when the service starts.

## Related pages

- [Configuration](Configuration) — the full `log` and `messageLog` sections.
- [Persistence](Persistence) — message and task history on disk.
- [Releases](Releases) — package layout and service management.
