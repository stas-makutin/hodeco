# Tasks

A task is a reusable automation: triggers start it, actions run sequentially, and
next rules can start other tasks. Tasks are protocol-agnostic; any protocol or
transport that hodeco supports can drive a message trigger or message action.

Tasks live in `tasks.yaml` (see [Configuration](Configuration)). The gateway
applies task changes through `tasks.*` immediately; there is no restart.

## Lifecycle

- Only one run is active per task. A trigger that arrives while the task is
  running is suppressed as `taskBusy`; it is not queued or replayed.
- A global `taskEngine.maxConcurrentRuns` cap suppresses triggers as
  `taskLimitExceeded`.
- Active runs are cancelled during service shutdown and the gateway waits briefly
  for them to finish before exiting.
- Disabled tasks cannot be started by `tasks.fire` or HTTP hooks. History is
  preserved. Enabling a task starts fresh from the current time.
- `tasks.purgeHistory` deletes a task's run rows without deleting the task.
- `runTimeout` cancels a run and its child processes. It covers the whole run,
  including an optional start delay. `failFast` stops after the first failed
  action; skipped actions are not written to history.
- `tasks.cancelRun` stops an active run by task and run ID. Cancellation kills a
  running command process, skips the remaining actions, and prevents its next
  rules from starting new tasks. Already-started child runs are not cancelled.
  The run is recorded as `cancelled` with a “cancelled by user” error, which
  distinguishes it from a `runTimeout` cancellation.
- `delay` waits before any action runs. `jitter` adds a deterministic symmetric
  offset, so a run waits for `delay ± jitter`. Jitter requires a delay and must
  not exceed it. The maximum combined delay is 24 hours.
- A delayed run is active immediately: it holds the task's single-run slot and
  counts toward `maxConcurrentRuns`. Triggers that arrive during the delay are
  suppressed as `taskBusy`. The delay applies to every start, including manual,
  API, and task-to-task child runs.
- `delayedUntil` in run summaries and `taskRunStarted` notifications identifies
  when actions begin. `runTimeout` counts from the run's start, so the wait
  consumes part of the run budget.

## Start delay

```yaml
delay: 5m
jitter: 30s
```

Both fields are optional. With `delay: 5m` and `jitter: 30s`, actions start after
a deterministic delay between 4m30s and 5m30s. The delay is part of the run, so
`runTimeout` must be longer than the maximum delay. Delay and jitter are checked
in the configuration validator and in the task editor.

## Triggers

```yaml
triggers:
  - id: motion
    type: message
    when: settled          # settled | new | stateChanged
    states: [incoming]
    protocols: [zwave]
    payload: { offset: 2, bytes: "ABM=" }

  - id: sunset
    type: schedule
    schedule: before-sunset
    maxRuns: 365

  - id: hook
    type: http
    methods: [POST]
    secret: optional
```

Message filters use the same semantics as `messages.list`: OR within each list,
AND between fields, with the shared payload expression evaluator. `settled`
fires incoming messages at registration and outgoing messages once at a terminal
state. `new` fires at registration. `stateChanged` fires on each transition.

Only one HTTP trigger is allowed per task. The hook URL is
`/api/hooks/{taskId}` and remains stable because task IDs are immutable.

Manual and API runs use `tasks.fire`. They do not need a trigger entry. Every
`tasks.fire` run is recorded with the `manual` trigger type; the optional
`triggerId` only replaces the history trigger ID (default `manual`) and must
name an existing configured trigger.

Runs started by next rules use the `task` trigger type and inherit the parent
trigger payload. They carry the parent run, chain, depth, and rule in
metadata/history.

## Actions

```yaml
actions:
  - id: meter
    command: meter
    args: ["--format", "json"]
    output: capture       # ignore | capture

  - id: notify
    message:
      service: { alias: ZW }
      payload: "AQID"
      ttl: 30s
```

Message action payloads are Base64-encoded bytes in JSON and YAML. In the UI,
message actions are configured with the Send message builder; service, payload,
and TTL are stored on the action. Z-Wave payloads are decoded back into the
builder when possible; other payloads open as Raw message. The builder's Raw
message field is the escape hatch for payloads that are not generated from a
protocol operation.

Commands run as the service account. Catalog arguments come first, then task
arguments, each as a separate argv entry. One JSON document is written to stdin.
Output is captured only with `output: capture`, capped at 8 KiB per run.
Captured stdout is raw bytes; the JSON API represents it as a Base64 string.
`stderr` is not captured. Selector stdin includes each action's Base64 `output`
and `outputTruncated` flag.

## Next rules

```yaml
next:
  - when: allSucceeded
    tasks: log-success
  - when: anyFailed
    tasks: alert
  - when: command-result
    command: classify
    args: []
    cases:
      0: success-task
      1: [notify, retry]
```

Conditions: `always`, `allSucceeded`, `anyFailed`, `allFailed`, and
`command-result`.

`command-result` runs the selector command. Exit codes `0..254` select a case;
`255` is a reserved selector error. Timeout, signal, spawn failure, and
cancellation are errors. An unmatched `0..254` starts nothing and is not an
error. A selector error marks the whole run failed. There is at most one
`command-result` rule per task. Targets are deduplicated per parent run, and only
children that actually start are recorded in the parent history.

`maxChainDepth` and `maxFanOut` bound chains.

## Commands and schedules

Commands and schedules are shared catalogs. Deleting a schedule or command that
is still referenced by a task is rejected with `scheduleInUse` or
`commandInUse`; the error includes the task IDs. Disabled tasks still count as
users. Remove the references first, then delete the catalog entry.

## Run feed and history

The service keeps a bounded **in-memory run feed** exposed by `tasks.runFeed`.
It holds the most recent completed runs from every task, up to 1,000 runs and
4 MiB. The oldest run is evicted when either limit is exceeded. Feed rows are
full run records, so `tasks.getRun` can resolve recent detail without reading
the history file. The feed clears on restart.

Task history is separate and opt-in. Set `history.maxRows` greater than zero to
enable it; `maxSize` must then be at least 1 KiB. Enabling it from the task
editor starts with 200 rows / 100 KiB; existing tasks keep their configured
limits.

When enabled, each task owns a history file under `tasks/`. Runs include
trigger metadata/payload, executed actions, selector result, started next tasks,
chain information, and the error. Persistent rows are not retained in service
memory; reads decode the file on demand. `tasks.purgeHistory` clears the file,
removes that task's feed rows, resets `maxRuns` counters, and clears the cached
last-run summary. History rows survive disabling a task.

With history disabled, completed runs exist only in the feed and are lost on
restart. The in-process last-run summary remains until restart.
Schedule-trigger `maxRuns` is a separate runtime counter and still works when
history is disabled.

`tasks.runFeed` exposes the feed with the same paging semantics as
`messages.list`: inclusive `from`, exclusive `until`, `order`, `limit`, and a
complete `next` bound pair. It has no manual page count.

`tasks.history` reads history files and uses the same paging semantics. It
supports task, trigger, trigger type, outcome, time, and order filters. The
full-page history view exposes trigger type, outcome, and period filters through
these request fields rather than filtering a local copy of the file.
`tasks.get` returns one task and its runtime status, including `runningRunId`
while a run is active; `tasks.getRun` checks the feed first, then the task's
history file.

Message-trigger detail records the message `state`; direction is derived from
that state and is not duplicated in run results or history files.

## UI

The Tasks tab contains Tasks, Schedules, Commands, and Runs. The task list uses
text actions (`Edit`, `Run`, `Cancel`, `History`, `Delete`), keeps its status row
visible, and renders empty/filter states inside the table area. Cancel is enabled
only while the task is running. The task editor is a full-page view with General,
Triggers, Actions, Next, and Limits & History log sections; all sections use
structured controls rather than JSON textareas. The header shows last and next
run plus suppressed counts; Run now is disabled while the task is disabled or
already running, and Limits & History log can reset defaults.
The Runs view is a windowed chronological feed: oldest first, newest last.
It shows 50 rows at a time and keeps at most 1,000 local rows. Scrolling to the
top loads older rows; scrolling to the bottom loads newer rows. New runs
autoscroll only while the user is at the bottom; otherwise the pending-run
counter stays visible. There is no page-size selector or manual load button.

Filters are task, trigger type, outcome (including **Running**), and period
(including **All time**). Running rows appear at the bottom; the **Control**
column holds **Cancel** for a running row.

Running rows appear at the bottom with a spinner. Running and completed rows
share one selection and keyboard model; single-click toggles the selected row
and its bottom detail pane, and **Close** or a second click clears it. Running
rows put **Cancel** in the **Control** column; finished rows show a dash. With a
running row selected, Tab moves focus to that row’s **Cancel** action;
subsequent Tab presses continue normal focus navigation. **Esc** clears the
selection and hides the detail pane, as in Messages. Running detail also offers
**Cancel run**.

The task row’s **History** action opens a full-page per-task history backed by
`tasks.history`. It uses the same 50-row window, chronology, keyboard navigation,
selection, and bottom autoscroll as Runs. Trigger type, outcome, and period
filters are sent to `tasks.history`; history rows are never loaded wholesale to
filter them in the UI. Selecting a row fetches the full run with `tasks.getRun`
and shows it in the bottom detail pane. Run details are grouped into General,
Trigger, Actions, Selector, Next tasks, Error, and Raw JSON sections; all
sections are collapsible.
