# WebSocket

The WebSocket is the UI's transport and the most convenient way to build a live
client. It carries the same RPC requests as `POST /api/rpc`, plus server-pushed
notifications on the same connection.

Connect to `GET /api/ws` with `ws://` or `wss://`. The browser UI connects to
`ws(s)://<page-host>/api/ws` by default.

## Subscribe first

No events are delivered until the client subscribes. The envelope matches the
HTTP RPC endpoint: a string `id`, a `method`, and an object of `params`.

```json
{"id":"1","method":"events.subscribe","params":{"topics":["messages","services"]}}
```

`events.unsubscribe` takes the same `topics` array. Valid topics are `messages`,
`discovery`, `services`, and `tasks`.

| Topic | Notifications |
| --- | --- |
| `messages` | `events.messageNew`, `events.messageDropped`, `events.messageStateChanged` |
| `discovery` | `events.discoveryStarted`, `events.discoveryFinished` |
| `services` | `events.serviceStateChanged` |
| `tasks` | `events.taskRunStarted`, `events.taskRunFinished` |

`events.taskRunStarted` carries `{ runId, taskId, trigger, startedAt }`.
`events.taskRunFinished` carries the run summary, including outcome, action and
next counts, and the error; fetch `tasks.get` for full action output.

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
