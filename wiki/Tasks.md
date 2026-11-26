# Tasks

A task is the "what should happen" part of hodeco automation. It is a reusable
recipe with three questions:

- **When?** One or more triggers start a run.
- **What?** The run executes actions in order.
- **Then?** Optional next rules can start other tasks based on the result.

Tasks are protocol-agnostic. Any protocol or transport that hodeco supports can
drive a message trigger or a message action.

Tasks live in `tasks.yaml` (see [Configuration](Configuration)). The gateway
applies changes made through the `tasks.*` methods immediately; there is no
restart.

If you are new to tasks, read [Your first task](#your-first-task), then the
sections on [triggers](#triggers), [actions](#actions), and
[next rules](#next-rules). The detailed lifecycle rules are grouped later under
[How runs behave](#how-runs-behave).

## Your first task

Let's make a task that runs on weekday evenings, polls a command, and then sends
a message. You need a command entry and a schedule entry first.

```yaml
# commands.yaml
commands:
  - id: meter
    name: Meter poll
    executable: /usr/local/bin/meter-poll
    args: ["--json"]
    timeout: 10s
```

```yaml
# schedules.yaml
schedules:
  - id: before-sunset
    name: Before sunset
    at: "sunset - 15m"
    days: [mon, tue, wed, thu, fri]
```

```yaml
# tasks.yaml
tasks:
  - id: evening-lights
    name: Evening lights
    triggers:
      - id: sunset
        type: schedule
        schedule: before-sunset
    actions:
      - id: meter
        command: meter
        output: capture
      - id: notify
        message:
          service: { alias: ZW }
          payload: "AQID"
```

Here is what happens:

1. The scheduler computes the next occurrence of `before-sunset` from
   `schedules.yaml`.
2. When it fires, the task starts a run with the `schedule` trigger.
3. The `meter` action runs the executable, appends the task's own arguments
   (none here), and captures its stdout.
4. The `notify` action sends the Base64 payload to service `ZW`.
5. The run finishes with an outcome. It appears in the **Runs** view and in the
   `tasks.runFeed` feed; if the task enables history, it is also written to the
   task's history file.

To try it:

- If you edited the YAML files directly, restart hodeco and use **Run now** in
  the UI to check it immediately.
- If you used the UI editors, the changes are already active.

The easiest way to watch what happened is the **Runs** view in the Tasks tab.
Select a run to see its trigger, actions, and error (if any).

## The shape of a task

| Field | Meaning |
| --- | --- |
| `id` | Immutable ASCII slug, unique in the catalog. |
| `name` | Display name. |
| `description` | Optional note. |
| `labels` | Optional free-form labels for task-list filtering. They do not affect execution. |
| `enabled` | Disabled tasks cannot be started by `tasks.fire` or HTTP hooks. Default `true`. |
| `triggers` | When the task starts. A task can have several. |
| `actions` | What the run does, in order. |
| `next` | Which tasks to start after this run, based on the result. |
| `runTimeout` | Whole-run timeout. Default `5m`; cancellation also kills child processes. |
| `failFast` | Stop after the first failed action. Default `true`. |
| `delay`, `jitter` | Optional start delay and deterministic offset. |
| `maxRunsPerMinute` | Rate limit for starts of this task. Default `60`. |
| `history` | Opt-in persistent run history. Disabled by default. |

A minimal task needs an ID and a name. In practice you will also add at least
one action; a task with no actions finishes without doing anything. Triggers are
optional if you only start it with `tasks.fire` or an HTTP hook.

```yaml
tasks:
  - id: ping-controller
    name: Ping controller
    actions:
      - id: ping
        command: ping-controller
```

## Labels

Labels organize a large task list. They are optional, free-form strings; a task
can have several. The task list shows them in a **Labels** column and filters by
them with OR semantics: selecting `lighting` and `automated` shows tasks that
have either label. The task search does not match labels.

Labels never change how a task runs and do not affect history. A task can have up
to 32 labels, each up to 32 characters, and labels must be unique within a task
case-insensitively.

Tools that create tasks through `tasks.add` should pass a label such as
`automated` or their own name so the admin can find their tasks:

```json
{
  "task": {
    "name": "Imported porch rule",
    "labels": ["automated", "n8n"]
  }
}
```

## Triggers

Triggers are the "when". A task can have more than one, and each trigger has an
`id` that is used in run history and filters. Only one HTTP trigger is allowed
per task.

### Schedule trigger

Reference a schedule from `schedules.yaml`:

```yaml
triggers:
  - id: sunset
    type: schedule
    schedule: before-sunset
    maxRuns: 365
```

`maxRuns` is an optional cap on actual starts for this trigger. It is counted in
memory and resets on gateway restart, task re-enable, and history purge. See
[Schedules](Schedules) for the schedule itself.

### Message trigger

A message trigger starts the task when an incoming or outgoing message matches
its filters:

```yaml
triggers:
  - id: motion
    type: message
    when: settled          # settled | new | stateChanged
    states: [incoming]
    protocols: [zwave]
    payload: { offset: 2, bytes: "ABM=" }
```

The filter fields work like `messages.list`: values within one list are ORed,
different fields are ANDed, and payload expressions use the shared evaluator.

| `when` value | Fires… |
| --- | --- |
| `settled` | For incoming messages at registration, and for outgoing messages once they reach a terminal state. |
| `new` | When the message is first registered. |
| `stateChanged` | On each state transition. |

`states` and `directions` narrow the match. `protocols`, `transports`,
`services`, and `payload` filter the source and contents. Task message triggers
ignore raw ZigBee ASH link frames; only EZSP and other protocol data messages can
start a task.

### HTTP trigger

```yaml
triggers:
  - id: hook
    type: http
    methods: [POST]
    secret: optional
```

The hook URL is `/api/hooks/{taskId}` and remains stable because task IDs are
immutable. The optional shared secret is sent in `X-Hodeco-Secret`. A successful
start returns `202` with a `runId`; `401` means a bad secret, `409` means the
task is disabled or busy, `413` means the payload is too large, and `429` means a
rate or concurrency limit. See [HTTP API](HTTP-API) for the request shapes.

### Manual and API runs

Manual and API runs use `tasks.fire`. They do not need a trigger entry. Every
`tasks.fire` run is recorded with the `manual` trigger type. The optional
`triggerId` only replaces the history trigger ID (default `manual`) and must
name an existing configured trigger.

### Child runs

Runs started by next rules use the `task` trigger type and inherit the parent
trigger payload. They carry the parent run, chain, depth, and rule in
metadata/history.

## Actions

Actions run sequentially. There are two kinds.

### Run a command

```yaml
actions:
  - id: meter
    command: meter
    args: ["--format", "json"]
    output: capture       # ignore | capture
```

The command must exist in `commands.yaml`. hodeco runs it as the service
account. The catalog arguments come first, then the task arguments, each as a
separate argv entry. One JSON document is written to stdin.

With `output: capture`, stdout is captured up to 8 KiB per run. Captured stdout
is raw bytes; the JSON API represents it as a Base64 string. `stderr` is not
captured. Selector stdin includes each action's Base64 `output` and
`outputTruncated` flag.

### Send a message

```yaml
actions:
  - id: notify
    message:
      service: { alias: ZW }
      payload: "AQID"
      ttl: 30s
```

Message payloads are Base64-encoded bytes in JSON and YAML. In the UI, message
actions are configured with the Send message builder; service, payload, and TTL
are stored on the action. Z-Wave payloads are decoded back into the builder when
possible; other payloads open as Raw message. The builder's Raw message field is
the escape hatch for payloads that are not generated from a protocol operation.

`ttl` is optional. Omit it to inherit the service's `outgoingMaxTTL` (10 s by
default); set it to `0 s` to disable expiry.

### Failure and cancellation

- With `failFast: true` (the default), the run stops after the first failed
  action. Skipped actions are not written to history.
- `runTimeout` cancels the run and its child processes. It covers the whole run,
  including an optional start delay.
- `tasks.cancelRun` stops an active run by task and run ID. Cancellation kills a
  running command process, skips the remaining actions, and prevents its next
  rules from starting new tasks. Already-started child runs are not cancelled.
  The run is recorded as `cancelled` with a "cancelled by user" error, which
  distinguishes it from a `runTimeout` cancellation.

## Next rules

Next rules run after the actions finish. They can start other tasks based on the
outcome:

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

| Condition | Starts the target when… |
| --- | --- |
| `always` | The run reaches the next-rule step, regardless of outcome. |
| `allSucceeded` | Every action succeeded. |
| `anyFailed` | At least one action failed. |
| `allFailed` | Every action failed. |
| `command-result` | A selector command's exit code maps to a configured case. |

`command-result` runs the selector command. Exit codes `0..254` select a case;
`255` is a reserved selector error. Timeout, signal, spawn failure, and
cancellation are errors. An unmatched `0..254` starts nothing and is not an
error. A selector error marks the whole run failed. There is at most one
`command-result` rule per task.

Targets are deduplicated per parent run, and only children that actually start
are recorded in the parent history. `maxChainDepth` and `maxFanOut` bound
chains; both are set in `taskEngine`.

## Runs: the live feed and history

There are two places to see completed runs, and they answer different questions.

| | In-memory run feed | Per-task history |
| --- | --- | --- |
| What it is | A bounded buffer of recent completed runs from every task. | An opt-in file per task. |
| Survives restart? | No. | Yes. |
| Default | Always on. | Off. |
| Limits | 1,000 runs and 4 MiB. | `history.maxRows` and `history.maxSize` per task. |
| API | `tasks.runFeed` | `tasks.history` |
| UI | **Runs** view | **History** action on a task |

The feed holds the most recent completed runs up to 1,000 runs and 4 MiB. The
oldest run is evicted when either limit is exceeded. Feed rows are full run
records, so `tasks.getRun` can resolve recent detail without reading a history
file. The feed clears on restart.

Task history is separate and opt-in. Set `history.maxRows` greater than zero to
enable it; `maxSize` must then be at least 1 KiB. Enabling it from the task
editor starts with 200 rows / 100 KiB; existing tasks keep their configured
limits.

When enabled, each task owns a history file under `tasks/`. Runs include trigger
metadata/payload, executed actions, selector result, started next tasks, chain
information, and the error. Persistent rows are not retained in service memory;
reads decode the file on demand. `tasks.purgeHistory` clears the file, removes
that task's feed rows, resets `maxRuns` counters, and clears the cached last-run
summary. History rows survive disabling a task.

With history disabled, completed runs exist only in the feed and are lost on
restart. The in-process last-run summary remains until restart.
Schedule-trigger `maxRuns` is a separate runtime counter and still works when
history is disabled.

### Reading runs through the API

`tasks.runFeed` exposes the feed with the same paging semantics as
`messages.list`: inclusive `from`, exclusive `until`, `order`, `limit`, and a
complete `next` bound pair. It has no manual page count.

`tasks.history` reads history files and uses the same paging semantics. It
supports task, trigger, trigger type, outcome, time, and order filters. The
full-page history view exposes trigger type, outcome, and period filters through
these request fields rather than filtering a local copy of the file.

`tasks.get` returns one task and its runtime status, including `runningRunId`
while a run is active. `tasks.getRun` checks the feed first, then the task's
history file.

Message-trigger detail records the message `state`; direction is derived from
that state and is not duplicated in run results or history files.

## How runs behave

These rules are the ones that matter when a task does not behave the way you
expected.

- **One run per task.** Only one run is active per task. A trigger that arrives
  while the task is running is suppressed as `taskBusy`; it is not queued or
  replayed.
- **Global concurrency cap.** `taskEngine.maxConcurrentRuns` suppresses triggers
  as `taskLimitExceeded`.
- **Start delay and jitter.** `delay` waits before any action runs. `jitter`
  adds a deterministic symmetric offset, so a run waits for `delay ± jitter`.
  Jitter requires a delay and must not exceed it. The maximum combined delay is
  24 hours.
- **A delayed run is already active.** It holds the task's single-run slot and
  counts toward `maxConcurrentRuns`. Triggers that arrive during the delay are
  suppressed as `taskBusy`. The delay applies to every start, including manual,
  API, and task-to-task child runs.
- **Time budget.** `delayedUntil` in run summaries and `taskRunStarted`
  notifications identifies when actions begin. `runTimeout` counts from the
  run's start, so the wait consumes part of the run budget.
- **Disabled tasks.** Disabled tasks cannot be started by `tasks.fire` or HTTP
  hooks. History is preserved. Enabling a task starts fresh from the current
  time.
- **Rate limit.** `maxRunsPerMinute` caps how often a task can start in a
  one-minute window.
- **Shutdown.** Active runs are cancelled during service shutdown and the
  gateway waits briefly for them to finish before exiting.
- **Purge.** `tasks.purgeHistory` deletes a task's run rows without deleting the
  task.

### Start delay and jitter in practice

```yaml
delay: 5m
jitter: 30s
```

Both fields are optional. With this example, actions start after a deterministic
delay between 4m30s and 5m30s. The delay is part of the run, so `runTimeout`
must be greater than `delay + jitter`. The configuration validator and the task
editor both check the limits.

## Managing tasks

### In the UI

The **Tasks** tab contains Tasks, Schedules, Commands, and Runs.

The task list uses text actions (`Edit`, `Run`, `Cancel`, `History`, `Delete`),
keeps its status row visible, and renders empty/filter states inside the table
area. Cancel is enabled only while the task is running. The header shows the
last and next run plus suppressed counts. **Run now** is disabled while the task
is disabled or already running.

The task editor is a full-page view with **General**, **Triggers**, **Actions**,
**Next**, and **Limits & History** sections. All sections use structured
controls rather than JSON textareas. **Limits & History** can reset defaults.

The **Runs** view is a windowed chronological feed: oldest first, newest last.
It shows 50 rows at a time and keeps at most 1,000 local rows. Scrolling to the
top loads older rows; scrolling to the bottom loads newer rows. New runs
autoscroll only while you are at the bottom; otherwise the pending-run counter
stays visible.

Filters are task, trigger type, outcome (including **Running**), and period
(including **All time**). Running rows appear at the bottom with a spinner. The
**Control** column holds **Cancel** for a running row. Running and completed
rows share one selection model: click a row to toggle the bottom detail pane,
and **Esc** or **Close** clears it. With a running row selected, Tab moves focus
to that row's **Cancel** action. Running detail also offers **Cancel run**.

The task row's **History** action opens a full-page per-task history backed by
`tasks.history`. It uses the same 50-row window, chronology, keyboard
navigation, selection, and bottom autoscroll as Runs. Trigger type, outcome, and
period filters are sent to `tasks.history`; history rows are never loaded
wholesale to filter them in the UI. Selecting a row fetches the full run with
`tasks.getRun` and shows it in the bottom detail pane. Run details are grouped
into General, Trigger, Actions, Selector, Next tasks, Error, and Raw JSON
sections; all sections are collapsible.

### From a file or the API

- Edit `tasks.yaml` and restart, or use the `tasks.*` methods to apply
  immediately.
- `tasks.add`, `tasks.update`, and `tasks.remove` rewrite the catalog file
  atomically.
- `tasks.fire` starts a manual run.
- `tasks.cancelRun` stops an active run.
- `tasks.runFeed`, `tasks.history`, and `tasks.getRun` read runs.
- `tasks.purgeHistory` clears a task's history and feed rows.

### References and deletion

Commands and schedules are shared catalogs. Deleting a schedule or command that
is still referenced by a task is rejected with `scheduleInUse` or
`commandInUse`; the error includes the task IDs. Disabled tasks still count as
users. Remove the references first, then delete the catalog entry.

## Related pages

- [Schedules](Schedules) — the "when" side in detail.
- [Configuration](Configuration) — `tasks.yaml`, `commands.yaml`, and the task
  engine limits.
- [HTTP API](HTTP-API) — `tasks.*` methods and HTTP hooks.
- [WebSocket](WebSocket) — `taskRunStarted`, `taskRunFinished`, and
  `taskRunDropped` notifications.
- [Persistence](Persistence) — how task history is stored on disk.
