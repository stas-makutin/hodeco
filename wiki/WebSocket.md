# WebSocket

The WebSocket is the live transport for the UI and API clients. It carries the
same RPC requests as `POST /api/rpc`, plus server-pushed notifications on the
same connection. If you are building a dashboard or anything that should react
to frames as they arrive, this is usually simpler than polling the HTTP API.

Connect to `GET /api/ws` with `ws://` or `wss://`. The browser UI connects to
`ws(s)://<page-host>/api/ws` by default.

## Connect and subscribe

No events are delivered until the client subscribes. The envelope matches the
HTTP RPC endpoint: a string `id`, a `method`, and an object of `params`.

```json
{"id":"1","method":"events.subscribe","params":{"topics":["messages","services"]}}
```

`events.unsubscribe` takes the same `topics` array. Valid topics are `messages`,
`discovery`, `services`, and `tasks`.

A typical client does four things after connecting:

1. Send `events.subscribe` with the topics you need.
2. Load the current state with the list methods (`services.list`,
   `messages.list`, `tasks.list`).
3. Consume notifications as they arrive.
4. On reconnect, repeat from step 1.

Subscribing does not replay missed events. The list methods are how you fill the
gap.

## What you receive

| Topic | Notifications |
| --- | --- |
| `messages` | `events.messageNew`, `events.messageDropped`, `events.messageStateChanged` |
| `discovery` | `events.discoveryStarted`, `events.discoveryFinished` |
| `services` | `events.serviceStateChanged` |
| `tasks` | `events.taskRunStarted`, `events.taskRunFinished`, `events.taskRunDropped` |

The `messages` topic includes both application frames and raw ZigBee ASH link
frames (ACK/NAK/RST/RSTACK/ERROR). Task message triggers ignore ASH link frames;
only EZSP and other protocol data messages can start a task.

`events.taskRunStarted` carries `{ runId, taskId, trigger, startedAt }` plus
`delayedUntil` when the run has a start delay.

`events.taskRunFinished` carries the run summary, including outcome, action and
next counts, and the error. Fetch `tasks.getRun` for full action output.

`events.taskRunDropped` carries `{ runIds }` when the bounded in-memory run feed
evicts rows or a task's feed rows are purged or removed.

`events.serviceStateChanged` reports runtime state only and never triggers a
config reload. After reconnecting, refresh with `services.list` rather than
assuming you missed nothing.

## Backpressure and liveness

The gateway is explicit about clients that fall behind:

- Each connection has a bounded outgoing queue of 256 messages. A slow consumer
  is closed with code `1013`.
- If the server-side event stream lags, the connection is also closed with
  `1013`.
- The server sends pings every 30 seconds and closes connections idle for
  60 seconds.

Treat `1013` as a resubscribe signal, not a fatal error.

## Reconnect recipe

1. Reconnect to `/api/ws`.
2. Send `events.subscribe` with the topics you need.
3. Refresh state with the list methods (`services.list`, `messages.list`,
   `tasks.list`, and discovery results if relevant).
4. Continue consuming notifications.

This is exactly what the bundled UI does; keeping the same sequence in your own
client avoids gaps after a restart or a slow-consumer close.

## Client checklist

- Use one connection for both RPC calls and notifications.
- Subscribe before you rely on events.
- Treat `1013` and a dropped connection the same way: reconnect, resubscribe,
  refresh.
- Do not assume notifications are replayed.
- Keep the list refresh bounded. Use the paging methods instead of loading the
  entire history on every reconnect.

## Related pages

- [HTTP API](HTTP-API) — the request/response side and the RPC methods.
- [Tasks](Tasks) — what task runs and outcomes mean.
- [Persistence](Persistence) — why the run feed is volatile and history is not.
