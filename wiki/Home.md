# Welcome to hodeco

hodeco shares the home devices attached to your serial controllers with the rest
of your local network. Z-Wave and ZigBee are supported today.

One hodeco process owns the controller. The bundled web UI, your scripts, and
your own automation all use the same typed JSON-RPC API, live WebSocket stream,
and searchable message history. You connect the controller once, then let every
local client share the same devices, state, and history instead of competing for
the serial port.

hodeco is meant to be quiet infrastructure: one process, a small set of YAML
files, and a browser. It runs on a Raspberry Pi, a mini-PC, or a Windows box,
with no external database and no cloud account.

> **Trusted networks only.** Authentication and TLS are not implemented, and the
> default bind address is `0.0.0.0`, so the UI and API listen on every network
> interface. Set `httpServer.bind` to `127.0.0.1` to restrict access to the local
> host. If you need access from outside your LAN, put hodeco behind a reverse
> proxy that provides both.

## New here? Start with a path

| If you want to… | Read |
| --- | --- |
| Understand what hodeco is before installing it | This page, then [How hodeco works](Concepts) |
| Run it and see the UI | [Getting Started](Getting-Started) |
| Connect a Z-Wave or ZigBee controller | [Getting Started](Getting-Started), then [Configuration](Configuration) |
| Automate lights, commands, or notifications | [Concepts](Concepts), then [Tasks](Tasks) and [Schedules](Schedules) |
| Write a script or integrate another app | [HTTP API](HTTP-API) and [WebSocket](WebSocket) |
| Change the protocol builder or device labels | [Codec Data Files](Data-Files) |
| Build it from source or contribute | [Development](Development) |

## The idea in one minute

A Z-Wave or ZigBee controller is a serial device. Normally only one program can
open its serial port. That is fine when you have one app, but it becomes a
problem as soon as you want a dashboard, a script, and an automation tool to see
the same devices.

hodeco solves that by becoming the one owner of the port:

1. You describe the controller once in `services.yaml`.
2. hodeco connects to it, reads frames, and keeps the device state and message
   history.
3. Every client talks to hodeco instead of the port — through the web UI, the
   HTTP API, or a WebSocket.
4. Tasks can react to messages, run commands, send messages, and start other
   tasks on a schedule.

You do not have to use every part. A minimal setup is one controller entry and
the UI. Automation, history, and the API are there when you need them.

## Try it in five steps

1. Plug a Z-Wave or ZigBee USB controller into the machine.
2. Install hodeco from a release archive or a `.deb`/`.rpm` package.
3. Start it and open <http://127.0.0.1:8180/>.
4. Add the serial service in the UI, or put it in `services.yaml`.
5. Watch frames in the **Messages** view, then add a task or connect a script.

The detailed version, including serial-port discovery and troubleshooting, is in
[Getting Started](Getting-Started).

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
                                                (Z-Wave and
                                                 ZigBee EZSP)
```

| Piece | What it is |
| --- | --- |
| `hodeco` | The gateway: HTTP/WebSocket API, service lifecycle, logging, message store, Z-Wave and ZigBee EZSP serial services. |
| `hodeco-contract` | The shared contract: wire types, method names, errors, configuration schema, parameter metadata, and the static protocol catalog. No Tokio or transport dependencies, so it also builds for WASM. |
| `hodeco-schedule` | Shared calendar, solar, repeat, and jitter computation used by the gateway scheduler and the UI preview/demo paths. |
| `hodeco-ui` | The egui admin and diagnostics frontend. Native and browser/WASM builds from the same code. |
| `hodeco-codec` | Pure Z-Wave and ZigBee EZSP/ZCL decoding, hex helpers, frame builders, command definitions, and entity catalog shared with the UI. |

## What it does

- **Shares the controller.** One service owns the serial port; the UI, scripts,
  and other local clients share the devices through the same API instead of
  competing for the port.
- **Talks to the controller** with a configurable baud rate and timeouts,
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
  JSON-driven protocol builder, and a demo mode that works without hardware.
- **Runs as a service.** Windows service and systemd integration are built in,
  including log and state directory handling.

## Supported controllers

| Protocol | Transport | Status |
| --- | --- | --- |
| Z-Wave | serial | Supported |
| ZigBee (Ember EZSP) | serial | Supported |

Each controller is one entry in `services.yaml`. The gateway owns the serial
port; clients share it through the API. New protocols are added behind the same
contract, so the API shape stays consistent as support grows.

## The map of the documentation

The pages are grouped by what you are trying to do, not by the order the code
was written.

**Learn the basics**

- [How hodeco works](Concepts) — the vocabulary and the mental model.
- [Getting Started](Getting-Started) — install, run, connect a controller.
- [Configuration](Configuration) — the five YAML files and what each field does.

**Use it day to day**

- [Tasks](Tasks) — triggers, actions, next rules, and run history.
- [Schedules](Schedules) — fixed times, solar events, repeats, and previews.
- [Persistence](Persistence) — what is stored, what survives a restart, and what
  to back up.
- [Logging](Logging) — the operational log versus the message history.

**Integrate with it**

- [HTTP API](HTTP-API) — endpoints, request shapes, methods, and recipes.
- [WebSocket](WebSocket) — subscriptions, notifications, and reconnection.
- [Codec Data Files](Data-Files) — the JSON formats behind the protocol builder.

**Project and releases**

- [Releases](Releases) — supported targets, archive layout, packages, upgrades.
- [Development](Development) — workspace commands, cross-checks, and licenses.

## Project status

The project is pre-1.0 (`0.3.1`). The protocol version is independent of the
crate version and is currently `1.0`. Z-Wave and ZigBee EZSP over serial are
supported today. New protocols are added behind the same shared contract, so the
API shape stays consistent as support grows.

## Project links

- [Source repository](../)
- [Published releases](../releases)
- [Release process](../blob/HEAD/RELEASING.md)
