# Changelog

All notable changes to hodeco are recorded here. The project follows semantic
versioning for the application release. The JSON-RPC protocol version is listed
separately because it can stay compatible across releases.

## 0.2.0 — 2026-11-21

Phase 2 adds a protocol-agnostic automation engine, reusable schedules and
commands, per-task run history, and the Tasks area of the UI.

### Breaking changes

- **Configuration is split into five files.** `config.yaml` no longer accepts a
  `services` list. Services, tasks, schedules, and commands live in
  `services.yaml`, `tasks.yaml`, `schedules.yaml`, and `commands.yaml` next to
  the core configuration. There is no migration from the old single-file shape;
  an old `services` key now fails startup with an unknown-field error.
- **The message-log format is incompatible.** Existing message-history files
  are not migrated, renamed, or read. With `messageLog.onCorrupt: ignore` (the
  default) an unsupported file is replaced at startup; with `onCorrupt: fail`
  startup is refused. Delete or archive the old message log before upgrading.
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
- **`tasks.get` now returns task info.** Run detail moved to `tasks.getRun`
  (`{ taskId, runId }`). This matches the other catalog `get` methods.
- **Command output is binary.** `actions[].command.stdout` is Base64-encoded
  bytes in JSON and in history. `stderr` is not captured. Selector stdin includes
  each command action's Base64 `output` and `outputTruncated` flag.
- **Task message actions no longer forward the trigger payload, and their payload
  is always Base64 bytes.** Task message actions must define a concrete
  `payload` string containing Base64-encoded bytes; there are no `source`/`text`
  variants. Compose it with the Send message builder. The trigger payload remains
  available to selectors and to `task`-type run history, but is not a message
  source.
- **Task definitions accept only message, schedule, and HTTP triggers.** Manual
  and API runs are always available and do not need a trigger entry. A task
  cannot declare another task as its trigger; task-to-task starts are expressed
  only in the source task's `next` rules. Run history still labels those runs
  with the `task` and `manual` trigger types.
- **Task history is opt-in and file-only.** Tasks without a `history` block no
  longer save completed runs. Set `history.maxRows` greater than zero to enable
  it; `history.maxSize` must then be at least 1 KiB. Persistent rows are no
  longer retained in service memory. The in-memory last-run summary remains
  available either way.
- **`tasks.history` uses `messages.list` paging.** The request no longer takes
  `next`; clients send the returned `from` / `until` bounds back. `tasks.getRun`
  resolves recent runs from the feed before falling back to the history file.
- **Message run triggers no longer include `direction`.** `state` already
  implies the direction, so the API and TLOG records omit it. There is no
  released history format to migrate.
- **`taskLog.maxSize` and `taskLog.maxRows` are removed.** Per-task history
  limits now live only in each task's `history.maxSize` / `history.maxRows`.
  `taskLog` keeps the directory, permissions, compaction, and corruption
  settings shared by every history file.
- **Schedule recurrence is flat.** The `on` and `loop` blocks are removed.
  `at`, `every`, `days`, `dayMatch`, `months`, and `years` are schedule-level
  fields. The old loop becomes a finite `repeatAt` list of offsets; there is no
  repeat count or unlimited repeat mode. Schedule jitter is a single top-level
  field applied to base and repeat occurrences.
- **Schedule repeats are fixed offsets.** `repeatAt: [30m, 1h]` runs once at
  30 minutes and once at 1 hour after each base occurrence, not 30 minutes and
  then 1 hour later. Repeats at or after the next base occurrence are skipped.
- **Schedule occurrences use `repeat`.** The occurrence kind `loop` is now
  `repeat`, and repeat occurrences include a one-based `repeatIndex`.
- **Multiple base cadences use multiple schedules.** A single schedule has one
  `every` value. Reference several schedules from a task instead of unioning
  cadences in one definition.

### Dependencies

- Added `chrono-tz` and `iana-time-zone` for IANA timezone and system-zone
  handling. Both are required for correct DST, calendar, and solar scheduling.
- `hodeco-ui` added `jiff` for civil date handling in the schedule date pickers.

### Configuration

- Added `files` to point at the four sibling catalogs.
- Added `taskLog` for settings shared by every task history file (`dir`,
  `fileMode`, `compactionFactor`, `compactInterval`, `onCorrupt`). Per-task
  retention is configured on each task's `history` block.
- Added `taskEngine` limits (`maxTriggerPayloadSize`, `maxConcurrentRuns`,
  `maxChainDepth`, `maxFanOut`) and the bounded `taskEngine.runFeed`
  (`maxRows`, `maxSize`).
- Added `messageLog.compactionFactor` and `messageLog.compactInterval`.
- `messageLog.autoPersist` now defaults to 60 seconds and controls how often
  message history is written to disk.
- `httpServer.maxConnections: 0` removes the practical connection cap.

### Automation

- Tasks with message, schedule, and HTTP triggers, always-available manual/API
  runs, and task-to-task starts from next rules.
- Sequential command and message actions with `failFast`, `runTimeout`, and
  optional 8 KiB-per-run output capture.
- Next-task rules: `always`, `allSucceeded`, `anyFailed`, `allFailed`, and
  `command-result` with exit codes `0..254` (`255` is reserved for selector
  errors).
- Reusable command catalog with JSON on stdin, fixed plus task arguments, an
  optional working directory, and a timeout.
- Reusable schedule catalog with fixed times, hourly minute marks, civil
  twilight and solar events, calendar intervals, day/month/year filters,
  allow/deny windows, exceptions, repeats, and deterministic jitter.
- Sunrise and sunset apply the observer elevation horizon-dip correction.
- HTTP hook at `/api/hooks/{task}` with optional `X-Hodeco-Secret`.
- Added the bounded in-memory run feed (`taskEngine.runFeed`, default 1,000 rows
  / 4 MiB). It holds recent runs from every task, evicts the oldest by rows or
  bytes, and clears on restart. `tasks.runFeed` exposes it with
  `messages.list` paging, and `events.taskRunDropped` reports evictions.
- Task history is opt-in and file-backed. Tasks without a `history` block do
  not save completed runs. When enabled, the service keeps only row/byte
  metadata and the latest summary per task; list and detail reads decode files
  on demand. `history.maxRows: 0` disables history for that task.
  Schedule-trigger `maxRuns` is counted in memory and is independent of history.
- Per-task `history.maxSize` and `history.maxRows` control history-file
  retention. Cleanup of files for removed tasks runs at startup.
- Repeats run once per `repeatAt` offset after every base occurrence, including
  a from-only one-shot. A repeat at or after the next base occurrence is
  skipped; `until` stops repeats at its boundary. Allow/deny suppress an
  attempt without changing the remaining offsets.
- Negative nth weekdays (`-1mon` … `-5mon`) are supported end to end.
- Repeat offsets with long calendar cadences use a previous-base lookback
  instead of the previous 31-day cap.
- `tasks.*`, `schedules.*`, and `commands.*` RPC methods, plus
  `events.taskRunStarted`, `events.taskRunFinished`, and
  `events.taskRunDropped` on the `tasks` topic.
- Tasks support an optional `delay` and symmetric `jitter`, capped at 24 hours
  combined. Delayed runs remain active, hold their concurrency slot, and count
  toward `runTimeout`. Run summaries and run-started events include
  `delayedUntil`.
- `schedules.setDefaults` updates the shared location and timezone in
  `schedules.yaml` and revalidates every existing schedule. `schedules.list`
  also returns the resolved host `systemTimezone`.
- `schedules.solarPreview` returns sunrise, sunset, dawn, dusk, solar noon,
  and nadir for a location and local date.

### UI

- Top-level navigation is now Services, Messages, Tasks, Configuration, Server,
  and Console; Services contains Services and Discovery sub-tabs.
- Tasks contains Tasks, Schedules, Commands, and Runs.
- The task list mirrors the Services table, with text row actions and stable
  empty/filter states.
- Full-page task and schedule editors, a command modal, and a Runs view with
  inline details.
- Configuration edits the shared `taskLog` block alongside the message log.
- Configuration also edits the catalog `files` paths and `taskEngine` safety
  limits, including the run feed, with grouped controls and inline field
  descriptions.
- Service parameter dialogs use the same muted help-text style.
- The Schedules page has a **Defaults** editor for shared location and timezone.
  Timezone uses **Default / Custom** radios with an inline searchable IANA
  dropdown; location uses **None / Custom** with a gated **Set location…** button.
  Save stays disabled until something changes. The schedule editor mirrors this
  with **Default / Custom** location labels.
- Location editing uses one shared two-pane dialog with coordinates and a
  solar-event preview filling the two halves of the dialog; the body scrollbar
  only appears when needed.
- Schedule **From** and **Until** use a `YYYY-MM-DD` field, **Pick date**, and a
  **Time** checkbox with hour/minute/second controls. Time controls stay visible
  but greyed when off. Dates use the schedule timezone; a checked time is
  converted to RFC 3339 for the API. Invalid drafts report in the fixed header.
- Date pickers can be closed and reopened reliably; clicking the button while a
  picker is open now closes it.
- Schedule and task jitter use one unlabelled row with **Add jitter**, `±`, and
  the shared number-plus-unit duration editor; disabled controls remain visible.
- Runs is a windowed chronological feed: 50-row pages, a 1,000-row local
  window, newest at the bottom, and autoscroll only while already at the bottom.
  The page-size selector and manual older-page button are gone. Active runs show
  a spinner and elapsed time; new rows while scrolled up are counted instead of
  forcing a scroll. The resizable bottom detail pane matches Messages.
- Running and completed runs share one selection and keyboard model. Clicking
  the selected row or **Close** clears the selection and hides the pane. Detail
  shows a hex payload and “Output: not captured” when capture is disabled. Row
  clicks use run identity like Messages, so filtered and running offsets cannot
  select the wrong row. Cell text is non-selectable so clicking any label
  selects the row, and selecting a run no longer locks Tab focus in the list.
- **Cancel active runs.** `tasks.cancelRun` takes a task and run ID and stops a
  delayed or running run: it kills the current command process, skips remaining
  actions and next rules, and records a `cancelled` run with a “cancelled by
  user” error distinct from timeout messages. The task list has a separate
  **Cancel** action, running rows put **Cancel** in the Runs **Control** column,
  and running-run detail has **Cancel run**. `tasks.get` and `tasks.list` expose
  `runningRunId` for the active run.
- The task row's **History** action opens a full-page per-task history backed by
  `tasks.history`, with the same keyboard navigation, selection, windowed
  paging, and bottom autoscroll as Runs. Enabling history log in the task editor
  starts at 200 rows / 100 KiB; existing tasks keep their configured limits.
- Task history adds Trigger, Outcome, and Period filters backed by
  `tasks.history`; the UI does not filter an in-memory copy of the history.
- Run detail is grouped into collapsible General, Trigger, Actions, Selector,
  Next tasks, Error, and Raw JSON sections, with aligned label/value fields.
- Message run triggers record `state` only. `direction` is derived from the
  state and is no longer included in run API results or history records.
- Runs adds **Running** to the outcome filter and **All time** to the period
  filter. The row control and next-task columns are labeled **Control** and
  **Next tasks**.
- Task, schedule, and command forms group related controls into bordered
  sections with inline field descriptions. Number-plus-unit controls preserve
  the entered number when the unit changes, adjusting only when a minimum would
  be violated.
- Task and schedule editors show validation and save errors in the fixed header
  and disable Save/Close while the draft is invalid.
- Task Limits & History adds full-width **Execution**, **Start delay**, and
  **History** groups. Start delay has enable and add-jitter checkboxes plus
  duration input/unit controls; jitter is a positive `±` magnitude. Runs waiting
  for their delay show a **Delayed** state and the history detail shows the delay
  window.
- Task trigger, action, and next-rule editors use structured cards; schedule
  recurrence and repeats use structured fields instead of JSON textareas.
- The recurrence section is always visible; there are no recurrence or loop
  enable checkboxes. Leaving At and Every empty makes the schedule a one-shot at
  **From**.
- Schedule recurrence uses time controls for fixed At entries, signed
  amount/unit solar offsets, an every checkbox with number/unit controls,
  add/remove year rows, and a structured special-day-rule builder. Elapsed-time
  every values of 24 hours or more, such as `30h` or `1800m`, are supported.
- Schedule previews are cleared when the timezone changes or a preview request
  fails, instead of showing stale occurrences. Exception date ranges now skip
  elapsed-time every occurrences and repeat attempts as well as calendar
  occurrences.
- Repeat uses ordered **Repeat after** offset rows with add/remove controls;
  there is no repeat count or unlimited mode. The editor states that repeats at
  or after the next scheduled occurrence are skipped. Allow/deny windows use
  separate from/to time controls, and exceptions use from/to date fields with
  pickers and add/remove.
- The special-day builder exposes `Last` and `2nd-last` … `5th-last` positions.
- Schedule and command editors auto-generate IDs from the name until the ID is
  edited, matching the task editor.
- The command modal uses the standard Cancel/Save footer; the redundant Discard
  button was removed. Its groups and fields fill the dialog width. Timeout uses
  the shared value-plus-unit editor, and each argument row reserves the
  **Remove** button width so the input plus button matches **Executable** and
  **Working directory**.
- Task editor content fills the workspace; Details stacks Name, ID, Description, and
  an Enabled checkbox with its help text. The schedule editor uses a flexible
  form with a compact 350 px rail; Preview and Used by are grouped sections
  without a vertical divider, and the rail scrolls as one. Preview puts Refresh
  on the title row, and empty Used by shows a muted **None**.
- Schedule previews show only the schedule timezone; the duplicate UTC column and
  UTC occurrence lines were removed. The demo gateway now uses the shared
  schedule engine for previews and next times instead of a synthetic hourly
  list.
- Message triggers now use protocol and transport drop-downs, explain the `when`
  mode, and share the full Messages payload-condition editor instead of the
  simple raw-byte dialog.
- A schedule trigger's run cap is labelled **Max runs**, matching the API field
  `maxRuns`.
- Message actions are configured in the Send message builder, where service,
  payload, and TTL are selected. New message actions have no stored service; the
  builder preselects the first configured service. Payload bytes are stored as
  Base64. Z-Wave payloads reopen in the builder when they decode; other payloads
  fall back to Raw message. The action card shows a **Compose message…** button
  followed by Service, TTL, the payload byte count, and the full payload hex.
  Command action and command-result selector argument fields use the same
  full-width rows; empty arguments are dropped on save.
- The Parsed message view shows each frame's byte count and decoded summary,
  every field's byte range, and TX options as named flags with the raw value.
- Task-editor multi-select dropdowns (next-rule targets and message-trigger
  protocols, transports, and services) stay open while checkboxes are toggled;
  click outside or press Esc to close them.
- Next-rule task selectors match the right edge of the rule's Remove button.
  Command-result task selectors use the same width and still leave room for
  their own Remove button.
- Task headers show next fire and suppressed counts, Run now respects
  disabled/running state, and Limits & History can reset defaults.
- Schedule day chips, row-based window/exception editors, and timezone-accurate
  preview replace the previous text-only controls.
- Next rules mark disabled targets, prevent duplicate exit-code cases, and
  enforce one command-result rule.
- HTTP triggers show their hook endpoint and only one can be added per task.
- Tab from a selected running row focuses that row’s **Cancel** action instead
  of jumping to the first focusable control on the page. **Esc** clears the Runs
  selection and hides the detail pane, matching Messages.

### Protocol

- Protocol version stays `1.0`. The new methods are additive at the transport
  level; the configuration shape change is an application/configuration break.

## 0.1.0

Initial public release.
