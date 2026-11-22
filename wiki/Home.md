# Welcome to hodeco

hodeco lets you share the home devices attached to your server's serial
controllers with the rest of your local network. One process owns the
controller, and the bundled web UI, your scripts, and your own automation all
use the same typed JSON-RPC API, live WebSocket stream, and searchable message
history. **Z-Wave is supported today; ZigBee is planned.**

That means no more fighting over the serial port: connect the controller once,
then let every local client share the same devices, state, and history. hodeco
is meant to be the quiet piece of infrastructure on a Raspberry Pi, a mini-PC,
or a Windows box: one process, a small set of YAML files, and a browser.

> **Trusted networks only.** Authentication and TLS are not implemented, and the
> default bind address is `127.0.0.1`. If you need access from outside your LAN,
> put hodeco behind a reverse proxy that provides both.

## What it does

- **Shares the controller.** One service owns the serial port; the UI, scripts,
  and other local clients share the devices through the same API instead of
  competing for the port. Currently Z-Wave; ZigBee is planned.
- **Talks to the controller** with configurable baud rate and timeouts,
  automatic reopen attempts, and live service state.
- **Keeps the history.** Every frame is stored in a bounded, file-backed message
  log you can page through, filter by time or metadata, and search with masked
  payload expressions.
- **Streams events.** Subscribe to `messages`, `discovery`, `services`, and
  `tasks` over a WebSocket. Slow consumers get a clear `1013` close instead of
  silently falling behind.
- **Ships its own UI.** The egui admin and diagnostics app is compiled to WASM
  and served by the same process. It includes services, discovery, message
  history, tasks/schedules/commands/runs, configuration, a console, a
  JSON-driven protocol builder with typed fields and packed bit groups, TX
  options, a per-send TTL override, and a demo mode that works without hardware.
- **Runs as a service.** Windows service and systemd integration are built in,
  including log and state directory handling.

## Supported controllers

| Protocol | Transport | Status |
| --- | --- | --- |
| Z-Wave | serial | Supported |
| ZigBee | serial | Planned |

Each controller is one entry in `services.yaml`. The gateway owns the serial
port, and clients share it through the API.

## A typical setup

1. Plug a Z-Wave USB controller into the machine.
2. Install hodeco from a release archive or a `.deb`/`.rpm` package.
3. Start it and open `http://127.0.0.1:8180/`.
4. Add the serial service in the UI, or put it in `services.yaml`.
5. Watch discovery, messages, and parsed frames arrive in the UI or over the
   WebSocket.

## How it fits together

```text
                    ┌─────────────────────────────┐
  browser / WASM UI │  static assets + WebSocket  │
                    └──────────────┬──────────────┘
                                   │
                            ┌──────▼──────┐
                            │   hodeco    │
                            └──┬───┬───┬──┘
                               │   │   │
             hodeco-contract ──┘   │   └── logs + message history
             (wire types,          │
              methods, schema)     └── serial ── controller
                                                (Z-Wave now,
                                                 ZigBee planned)
```

| Piece | What it is |
| --- | --- |
| `hodeco` | The gateway: HTTP/WebSocket API, service lifecycle, logging, message store, Z-Wave serial service. |
| `hodeco-contract` | The shared contract: wire types, method names, errors, configuration schema, parameter metadata, and the static Z-Wave/serial catalog. No Tokio or transport dependencies, so it also builds for WASM. |
| `hodeco-schedule` | Shared calendar, solar, repeat, and jitter computation used by the gateway scheduler and the UI preview/demo paths. |
| `hodeco-ui` | The egui admin and diagnostics frontend. Native and browser/WASM builds from the same code. |
| `hodeco-codec` | Pure Z-Wave/Parsed decoding, hex helpers, frame builder, command definitions, and entity catalog shared with the UI. |

## Start here

- [Getting Started](Getting-Started) — run a release, build from source, and do
  a first-run check.
- [Configuration](Configuration) — the full `config.yaml` reference and platform
  defaults.
- [Tasks](Tasks) — triggers, actions, next rules, hooks, and run history.
- [Schedules](Schedules) — recurrence, solar events, repeats, and previews.
- [Persistence](Persistence) — message and task-history storage.
- [HTTP API](HTTP-API) — endpoints, request shapes, methods, and static assets.
- [WebSocket](WebSocket) — subscriptions, notifications, and backpressure.
- [Logging](Logging) — the operational log versus the message history.
- [Releases](Releases) — supported targets, archive layout, and packages.
- [Development](Development) — workspace commands, cross-checks, and licenses.
- [Codec Data Files](Data-Files) — formats of `zwave.json` and `entities.json`.

## Project status

The project is pre-1.0 (`0.2.0`). The protocol version is independent of the
crate version and is currently `1.0`. Z-Wave over serial is supported today;
ZigBee is planned. New protocols are added behind the same shared contract, so
the API shape stays consistent as support grows.

## Project links

- [Source repository](../)
- [Published releases](../releases)
- [Release process](../blob/HEAD/RELEASING.md)
