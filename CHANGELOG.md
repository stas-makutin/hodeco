# Changelog

All notable changes to hodeco are recorded here. The project follows semantic
versioning for the application release. The JSON-RPC protocol version is listed
separately because it can stay compatible across releases.

## 0.3.1 — 2026-11-26

Patch release with run-history, Z-Wave builder, restart, and first-run access
fixes. There are no configuration or message-log format breaks.

### Added

- **Live rows in per-task history.** `tasks.history` accepts
  `includeFeed: true`. Matching rows from the in-memory run feed are merged with
  file rows and deduplicated by run ID before paging. The History view sets this
  flag, so recent runs remain visible for tasks without persistent history.
- **AUTO_ROUTE by default.** Z-Wave SendData-style builder operations select
  `ACK` and `AUTO_ROUTE` by default. The defaults come from the command catalog
  and can still be changed per message.
- **Thermostat Setpoint types.** Added Away heating, Away cooling, and Full
  power labels for the newer setpoint type values.

### Changed

- **Server restart now restarts the gateway.** `system.restart` performs a
  graceful shutdown and starts the gateway again instead of only stopping it.
  Console runs start a replacement process, systemd uses its failure-restart
  policy, and the Windows service restarts through the service manager. The
  Server tab and confirmation dialog explain the new behavior.
- **Default HTTP bind is now `0.0.0.0`.** The UI and API accept connections on
  every IPv4 interface by default. Set `httpServer.bind: 127.0.0.1` for
  loopback-only access. Authentication and TLS are still not implemented.
- **Blank catalog IDs stay blank until save, then the server generates them.**
  The task, schedule, and command editors no longer refill an intentionally
  cleared ID field while typing. IDs are still derived from the name by default.

### Fixed

- **Z-Wave Thermostat Setpoint size parsing and building.** The size field now
  uses the wire values `1`, `2`, and `4` for 1-, 2-, and 4-byte values instead
  of `0`, `1`, and `2`. One- and two-byte reports decode correctly, and the
  builder emits the matching precision/scale/size byte and value width.
- **Per-task history for tasks without persistent history.** The History action
  now shows recent in-memory runs instead of an empty page when the task has no
  TLOG file. Rows that also exist in the file are returned once.
- **Browser ID generation.** IDs generated in the web UI use `web-time` instead
  of `std::time::SystemTime`, which panics on `wasm32-unknown-unknown`.
- **Z-Wave size-field regression coverage.** Added a catalog test covering the
  size mappings for Thermostat Setpoint, Multilevel Sensor, Meter, Humidity
  Control Setpoint, and Configuration.

### Protocol

- Protocol version stays `1.0`. `tasks.history` adds the optional `includeFeed`
  request field; it defaults to `false`, so existing clients keep file-only
  behavior.

## 0.3.0 — 2026-11-25

Phase 3 adds ZigBee support over Ember EZSP serial coordinators, with the same
raw-relay model as Z-Wave, and improves the Z-Wave builder and filter. There are
no configuration or message-log format breaks in this release.

### Added

- **ZigBee (Ember EZSP) services.** `protocol: zigbee` over `transport: serial`.
  The gateway owns ASH framing, acknowledgements, retries, reset, reconnect, and
  EZSP version negotiation; clients send and receive canonical EZSP frames:
  `seq | frameControlLow | frameControlHigh=0x01 | frameId (LE) | parameters`.
  Discovery negotiates EZSP and reports the stack version. The HUSBZB-1 ZigBee
  side defaults to 57600 8N1.
- **Serial flow control.** `flowControl` accepts `none`, `software`, or
  `hardware` and defaults to `none`.
- **ZigBee link tuning.** `ashAckTimeout` (default 1.6 s) and `ashMaxRetries`
  (default 5) control ASH acknowledgement timing and retry count; the gateway
  reopens the link when the retry count is exhausted.
- **ASH link diagnostics.** ASH ACK/NAK/RST/RSTACK/ERROR frames are stored as raw
  wire messages in the message history. A missing acknowledgement is visible as
  an outgoing EZSP request that never reaches the acknowledged state, with no
  corresponding link ACK. ASH link frames never trigger tasks.
- **ZigBee message decoding and builder.** EZSP request, response, and callback
  layouts, nested ZCL cluster commands and attributes, and raw ASH link-frame
  decoding. Curated clusters include On/Off, Level, Color Control, Groups,
  Scenes, Identify, and the common measurement clusters. The builder covers
  coordinator commands such as `networkInit`, `formNetwork`, `permitJoining`,
  `leaveNetwork`, `sendUnicast`, `sendBroadcast`, and `sendMulticast`, with typed
  little-endian fields. Send commands can compose a ZCL payload with cluster and
  command pickers, general ZCL commands, direction-aware fields, manufacturer
  codes, typed payload fields, and known-device NWK/endpoint selection. Fielded
  requests can be restored from decoded frames; raw EZSP parameters and raw ZCL
  bytes remain available as fallbacks. Unknown commands and clusters fall back
  to raw bytes.
- **ZigBee payload conditions.** Match the EZSP command, network address, group,
  cluster, endpoint, other fixed EZSP fields, raw EZSP parameters, ZCL frame
  control/direction, general or cluster ZCL commands, typed ZCL fields, and raw
  ZCL payloads. A Request/Response switch selects the EZSP field layout. Fields
  that the selected command does not carry are disabled and cleared, so an
  address is not left attached to `permitJoining` or another non-addressed
  command. Group ID matching covers `sendMulticast` and group-addressed
  `incomingMessageHandler` frames.
- **ZigBee entity labels and codec data.** `entities.json` accepts 16-bit NWK
  addresses, optional EUI-64, endpoint, and profile metadata. The parsed view
  labels known sender/destination addresses, and the builder and filter offer
  known-device pickers. Legacy `device_id` and `service_id` keys remain accepted
  as aliases. `zigbee.json` provides runtime-editable EZSP commands, general ZCL
  commands, ZCL frame-control metadata, and ZCL cluster definitions, loaded like
  `zwave.json`.
- **Service status detail.** `ServiceStatus.detail` optionally carries
  protocol-specific information such as `EZSP v4 · stack 0x5410`.
- **Z-Wave builder and filter improvements.** TX option checkboxes come from the
  serial command's `txOptions` values, and broadcast TX options are fixed.
  Command class and command pickers list catalog values only; the **Match in
  filter** checkbox controls whether the selected value participates. New
  Z-Wave frame conditions start at Basic Set, so the common case needs no extra
  selection. Command-class payloads can optionally be matched with typed
  fixed-width fields; only checked fields participate, and raw command data
  remains the default or an extra tail when typed fields are enabled. The node
  field offers known nodes from `entities.json`, and typed command fields use
  enum, flag, boolean, and numeric editors instead of free-form text.
- **Z-Wave catalog corrections.** Display names such as Switch Binary, Switch
  Multilevel, Scene Activation, Start Level Change, and Meter Supported Get now
  use normal spacing. Classes without named command definitions remain usable:
  the builder accepts raw command bytes with the class byte added automatically,
  and the filter accepts a numeric command ID with raw parameters.
- **Demo fixtures.** ZigBee service, discovery, EZSP, ASH, and ZCL messages,
  including `permitJoining`, `sendUnicast`, On/Off, Level, Color Control, and
  attribute reports. The entity catalog maps several ZigBee NWK addresses,
  endpoints, profiles, and EUI-64 values; discovery also includes unconfigured
  serial ports that can be added.
- **Linux packaging.** The `.deb` and `.rpm` packages now install a systemd unit,
  a sysusers definition, a default `config.yaml`, and maintainer scripts. They
  create or repair the service account, add serial groups when present, prepare
  the runtime directories, and start or stop the unit on install, upgrade, and
  removal.
- **ARMv6 release target.** `arm-unknown-linux-musleabihf` covers Raspberry Pi 1,
  Zero, Zero W, and 32-bit Raspberry Pi OS.
- **Task labels.** Tasks accept optional free-form `labels`. The Tasks UI shows a
  Labels column and a searchable label filter; selecting several labels matches
  tasks with any of them. Labels are organizational only and do not affect
  execution. Tools that create tasks through `tasks.add` can label them with
  `automated` or their own name.

### Changed

- **ZigBee discovery timing.** Discovery now allows the full EZSP reset and
  version-negotiation handshake instead of reusing the shorter Z-Wave probe
  budget, so HUSBZB-1 and similar coordinators are detected reliably.
- **ZIP log archives by default.** Rotated operational logs use ZIP compression
  by default. Set `log.archive: none` to keep rotated files as plain files.
- **Package-managed systemd unit.** When the service is installed by a package,
  `hodeco install` and `hodeco uninstall` leave it alone and point to the package
  manager instead of creating or removing a manual unit.

### UI

- The UI endpoint option is now `--gateway-url` and `?gateway_url=`. The previous
  `--server` and `?server=` forms remain accepted as legacy aliases.
- Failed dialog actions show a truncated error beside the buttons in the fixed
  footer, so a long scrollable body cannot hide it.

### Protocol

- Protocol version stays `1.0`. The new fields and capabilities are additive;
  the core configuration shape is unchanged. `tasks.yaml` gains an optional
  `labels` field; older binaries reject it because unknown fields are rejected.

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

## 0.1.0 — 2026-11-15

Initial public release.
