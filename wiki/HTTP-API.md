# HTTP API

The API is deliberately small: one RPC endpoint, one WebSocket, one health
check, and the static UI. Everything the browser does goes through the same
routes available to your own scripts.

| Route | Purpose |
| --- | --- |
| `POST /api/rpc` | JSON-RPC-style request/response. |
| `GET /api/ws` | WebSocket for RPC calls and live notifications. |
| `GET /api/health` | Health check. |
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
    "version": "0.1.0",
    "protocolVersion": "1.0",
    "startedAt": "2026-01-01T12:00:00Z"
  }
}
```

Transport failures use HTTP status codes: malformed JSON is `400`, an oversized
body is `413`, and the wrong content type is `415`. Method-level failures — an
unknown method, invalid params, a missing service — come back as an error object
in the response body with HTTP `200`.

## RPC methods

The method names live in `hodeco-contract`:

- System and server: `system.restart`, `server.info`
- Configuration: `config.get`, `config.update`
- Metadata: `protocols.list`, `transports.list`, `protocols.info`,
  `protocols.params`
- Discovery: `discovery.start`, `discovery.get`, `discovery.cancel`
- Services: `services.list`, `services.add`, `services.remove`,
  `services.alias`, `services.status`, `services.params`, `services.send`
- Messages: `messages.get`, `messages.list`
- Events: `events.subscribe`, `events.unsubscribe`

`server.info` returns the crate version (shown as `version`), the independent
`protocolVersion` (currently `1.0`), and `startedAt`.

`messages.list` is the workhorse: timestamp-bounded paging, metadata and ID
filters, and protocol-neutral masked `PayloadExpr` trees evaluated by the
gateway, not the browser. `messages.get` fetches one message by ID.

`services.send` takes a service reference, a base64 `payload`, and an optional
`ttl` duration such as `"500 ms"` or `"10 s"`. Omit `ttl` to inherit the
service's `outgoingMaxTTL` (10 s by default), then the protocol default. The UI
builder exposes the same choice as **Override TTL**.

The exact request/result shapes are defined by the `hodeco-contract` crate and
are exercised by its contract tests.

## Static assets

Static assets are served from `httpServer.static.dir` with:

- mtime-based cache validation (`Last-Modified` and `If-Modified-Since`);
- optional gzip and Brotli sidecars when `precompressed: true`;
- COOP/COEP headers;
- SPA fallback for extensionless navigation paths when `spaFallback: true`;
- `404` for missing asset files such as `.wasm`, `.js`, and `.css`.

JSON definition files are always served uncompressed so runtime edits are not
masked by stale sidecars. Release builds include raw assets plus `.br` and `.gz`
sidecars; clients that accept neither encoding still receive the raw file.

## Health and readiness

`GET /api/health` reports healthy while the server is running. During shutdown,
new RPC requests receive a `serverShuttingDown` error while existing work is
drained.
