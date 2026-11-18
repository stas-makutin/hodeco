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
- Disabled tasks cannot be started by `tasks.fire` or HTTP hooks. History is
  preserved. Enabling a task starts fresh from the current time.
- `tasks.purgeHistory` deletes a task's run rows without deleting the task.
- `runTimeout` cancels a run and its child processes. `failFast` stops after the
  first failed action; skipped actions are not written to history.

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

Manual and API runs use `tasks.fire`. The optional `triggerId` selects an
existing trigger for history; otherwise the trigger ID is `manual`.

Task triggers are produced by next rules. They inherit the parent trigger
payload and carry the parent run, chain, depth, and rule in metadata/history.

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
      payload: { source: trigger }   # or text/hex/base64
      ttl: 30s
```

Commands run as the service account. Catalog arguments come first, then task
arguments, each as a separate argv entry. One JSON document is written to stdin.
Output is captured only with `output: capture`, capped at 8 KiB per run.

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

## History

Each task owns `tasks/<id>.tlog`, an append-only TLOG file with typed blocks.
Runs include trigger metadata/payload, executed actions, selector result,
started next tasks, chain information, and the error. The default limit is
100 KiB / 1,000 rows per task; `tasks.purgeHistory` clears it and resets
`maxRuns` counters. History rows survive disabling a task.

Set `history.maxRows: 0` to disable history for a task. Completed runs are not
appended to TLOG; `maxSize` is ignored. The in-process last-run summary remains
visible until restart. Schedule-trigger `maxRuns` is a separate runtime counter
and still works when history is disabled.

`tasks.history` supports task, trigger, trigger type, outcome, time, and order
filters, with the same timestamp cursor as `messages.list`. `tasks.get` returns
one full run.

## UI

The Tasks tab contains Tasks, Schedules, Commands, and Runs. The task list uses
text actions (`Edit`, `Run`, `History`, `Delete`), keeps its status row visible,
and renders empty/filter states inside the table area. The task editor is a
full-page view with General, Triggers, Actions, Next, and Limits & History
sections. The Runs view shows live finish events and an inline detail pane.
