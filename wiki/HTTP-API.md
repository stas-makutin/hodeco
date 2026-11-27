# HTTP API

hodeco has one RPC endpoint, one WebSocket, one health check, task hooks, and
the static UI. Everything the browser does goes through the same routes that are
available to your own scripts.

If you only use the web UI, you do not need this page. If you are writing a
script or another app, start with [A first request](#a-first-request), then use
the [method map](#method-map) to find the call you need.

| Route | Purpose |
| --- | --- |
| `POST /api/rpc` | JSON-RPC-style request/response. |
| `GET /api/ws` | WebSocket for RPC calls and live notifications. |
| `GET /api/health` | Health check. |
| `GET/POST /api/hooks/{task}` | Starts a task's HTTP trigger. |
| `GET /*` | Static frontend assets. |

RPC requests use `application/json`. The request body limit is 512 KiB and is
shared with WebSocket messages. The header limit, connection limit, header-read
timeout, idle timeout, and request/response body timeouts are enforced from the
`httpServer` config section.

## A first request

The envelope is JSON-RPC-flavored but minimal: `id` is a string, `method` is a
known name, and `params` is an object. The `jsonrpc` field is not used.

```sh
curl -s http://127.0.0.1:8180/api/rpc \
  -H 'content-type: application/json' \
  -d '{"id":"1","method":"server.info","params":{}}'
```

A successful response looks like:

```json
{
  "id": "1",
  "result": {
    "version": "0.3.1",
    "protocolVersion": "1.0",
    "startedAt": "2026-01-01T12:00:00Z"
  }
}
```

`server.info` returns the crate version (shown as `version`), the independent
`protocolVersion` (currently `1.0`), and `startedAt`.

## Responses and errors

There are two layers of failure:

- **Transport failures** use HTTP status codes. Malformed JSON is `400`, an
  oversized body is `413`, and the wrong content type is `415`.
- **Method-level failures** — an unknown method, invalid params, a missing
  service — come back as an error object in the response body with HTTP `200`.

An error response looks like this:

```json
{
  "id": "2",
  "error": {
    "code": "serviceNotFound",
    "message": "service not found"
  }
}
```

Always check for `error` before reading `result`.

## Method map

The method names live in `hodeco-contract`. The groups below match the way the
UI and the documentation are organized.

| Area | Methods | Related |
| --- | --- | --- |
| System and server | `system.restart`, `server.info` | [Configuration](Configuration) |
| Configuration | `config.get`, `config.update` | [Configuration](Configuration) |
| Metadata | `protocols.list`, `transports.list`, `protocols.info`, `protocols.params` | [Configuration](Configuration) |
| Discovery | `discovery.start`, `discovery.get`, `discovery.cancel` | [Getting Started](Getting-Started) |
| Services | `services.list`, `services.add`, `services.remove`, `services.alias`, `services.status`, `services.params`, `services.send` | [Configuration](Configuration) |
| Messages | `messages.get`, `messages.list` | [Logging](Logging), [Data-Files](Data-Files) |
| Tasks | `tasks.list`, `tasks.get`, `tasks.getRun`, `tasks.add`, `tasks.update`, `tasks.remove`, `tasks.fire`, `tasks.cancelRun`, `tasks.runFeed`, `tasks.history`, `tasks.purgeHistory` | [Tasks](Tasks) |
| Schedules | `schedules.list`, `schedules.get`, `schedules.add`, `schedules.update`, `schedules.remove`, `schedules.preview`, `schedules.setDefaults`, `schedules.solarPreview` | [Schedules](Schedules) |
| Commands | `commands.list`, `commands.get`, `commands.add`, `commands.update`, `commands.remove` | [Tasks](Tasks) |
| Events | `events.subscribe`, `events.unsubscribe` | [WebSocket](WebSocket) |

`discovery.*` is a serial-port scan, not device pairing: it probes the host's
ports with the selected protocol and reports which entries respond, so you can
choose an `entry` for a service.

`messages.list` is the workhorse: timestamp-bounded paging, metadata and ID
filters, and protocol-neutral masked `PayloadExpr` trees evaluated by the
gateway, not the browser. `messages.get` fetches one message by ID.

## Common recipes

### List services and check their state

```json
{"id":"1","method":"services.list","params":{}}
```

The result contains a `services` array. Each entry includes the service alias,
protocol, transport, entry, explicit parameter overrides, and runtime status. Use
`services.status` for one service. The status includes an optional `detail`
string; ZigBee services use it for the negotiated EZSP version, for example
`EZSP v4 · stack 0x5410`.

### Read the recent messages

```json
{
  "id": "1",
  "method": "messages.list",
  "params": {
    "limit": 50
  }
}
```

The result includes `messages`, a `storeTotal`, and a `next` object with
absolute `from`/`until` bounds for the next page. `order` is `0` for
newest-first (the default) and `1` for oldest-first.

### Send a message

```json
{
  "id": "1",
  "method": "services.send",
  "params": {
    "service": {"alias": "ZW"},
    "payload": "AQID",
    "ttl": "30 s"
  }
}
```

`payload` is Base64-encoded bytes. `ttl` is optional: omit it to inherit the
service's `outgoingMaxTTL` (10 s by default); set it to `0 s` to disable expiry.
The UI builder exposes the same choice as **Override TTL**.

ZigBee payloads are canonical EZSP frames:

```text
seq | frameControlLow | frameControlHigh = 0x01 | frameId (2, LE) | parameters
```

The gateway adds ASH framing, CRC, escaping, and ACK handling. ASH ACK/NAK are
handled by the gateway; the gateway also records link frames as raw message
payloads for diagnostics. `services.send` preserves the caller's EZSP frame
exactly; it does not rewrite the sequence.

### Create a labeled task

Tools that create tasks can attach free-form labels. The UI shows them in the
task list and filters by them; they do not affect execution.

```json
{
  "id": "1",
  "method": "tasks.add",
  "params": {
    "task": {
      "name": "Imported porch rule",
      "labels": ["automated", "n8n"]
    }
  }
}
```

A task accepts up to 32 labels, each up to 32 characters. Labels are trimmed and
must be unique within a task, case-insensitively.

### Start a task manually

```json
{
  "id": "1",
  "method": "tasks.fire",
  "params": {"taskId": "evening-lights"}
}
```

The result contains a `runId`. The optional `triggerId` replaces the history
trigger ID (default `manual`) and must name an existing configured trigger. An
optional `payload` and `metadata` are carried into the run.

### Call an HTTP task hook

HTTP hooks are the one non-RPC endpoint:

```sh
curl -i -X POST http://127.0.0.1:8180/api/hooks/evening-lights \
  -H 'X-Hodeco-Secret: optional'
```

`GET|POST /api/hooks/{task}` starts the task's HTTP trigger and returns `202`
with a `runId`. A bad secret is `401`, a disabled or busy task is `409`, an
oversized payload is `413`, and a rate or concurrency limit is `429`.

## Paging

`messages.list`, `tasks.runFeed`, and `tasks.history` share the same paging
shape:

- `from` is inclusive;
- `until` is exclusive;
- `order` selects the direction;
- `limit` caps the page;
- the response returns a complete `next` bound pair, or omits it at the end.

Requests do not take a `next` field. Use the returned `next.from` and
`next.until` values for the following request.

`tasks.runFeed` reads the volatile in-memory recent-run buffer;
`tasks.history` reads persistent per-task history files. Both accept `taskIds`,
`triggerIds`, `types`, and `outcomes` filters; the service applies them before
paging. Set `includeFeed: true` on `tasks.history` to merge matching feed rows
and remove duplicate run IDs before paging; the UI uses this so recent runs
remain visible for tasks without persistent history. `tasks.getRun` checks the
feed first and falls back to the task's history file.

## Static assets

Static assets are served from `httpServer.static.dir` with:

- mtime-based cache validation (`Last-Modified` and `If-Modified-Since`);
- optional gzip and Brotli sidecars when `precompressed: true`;
- COOP/COEP headers;
- SPA fallback for extensionless navigation paths when `spaFallback: true`;
- `404` for missing asset files such as `.wasm`, `.js`, and `.css`.

JSON definition files are read from the raw file rather than a precompressed
sidecar, so runtime edits are not masked by stale sidecars; the outer compression
layer may still compress the response on the fly. Release builds include raw
assets plus `.br` and `.gz` sidecars; clients that accept neither encoding still
receive the raw file.

## Health and readiness

`GET /api/health` reports healthy while the server is running. During shutdown,
new RPC requests receive a `serverShuttingDown` error and HTTP hooks return
`503` while existing work is drained.

## Where the exact shapes live

The exact request and result shapes are defined by the `hodeco-contract` crate
and are exercised by its contract tests. If this page and the code disagree, the
crate wins. The method names are in
`hodeco-contract/src/api/methods.rs`, and the payload structs are in
`hodeco-contract/src/api/payloads.rs`.

## Related pages

- [WebSocket](WebSocket) — the live transport and notification subscriptions.
- [Tasks](Tasks) — task semantics behind `tasks.*` and HTTP hooks.
- [Schedules](Schedules) — `schedules.preview` and recurrence rules.
- [Configuration](Configuration) — `config.get` and `config.update`.
