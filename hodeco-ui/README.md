# hodeco-ui

`hodeco-ui` is the egui-based admin and diagnostics frontend for the
[`hodeco`](../README.md) gateway. It is a separate Cargo workspace under
`hodeco-ui/` and uses `hodeco-contract` for the wire protocol,
`hodeco-codec` for decoding and frame building, and `hodeco-schedule` for
schedule previews and demo fixtures.

You usually do not need to build it. Release archives and packages include the
compiled web UI, and the gateway serves it at the same address as the API.

- New to hodeco? Start with [Getting Started](../wiki/Getting-Started.md) and
  the [wiki home](../wiki/Home.md).
- Working on the UI? Jump to [Run natively](#run-natively) and
  [Build the web bundle](#build-the-web-bundle).
- Looking for the protocol definitions? See
  [Codec Data Files](../wiki/Data-Files.md).

## What is in the UI

The UI is an admin and diagnostics app, not a separate service. It connects to
the gateway over one WebSocket and covers:

**Operate**

- Services and discovery, with parameter editors that use the same `ParamDef`
  metadata as the gateway.
- Message history with a bounded local list, gateway-side filtering, and
  older/newer paging.
- A Send message builder driven by the protocol JSON definitions.
- Tasks, schedules, commands, and runs, including task labels and filtering, the
  run feed, and per-task history.
- Configuration, server details, and a console.
- Theme switch, banners, toasts, and reconnect handling.

**Inspect**

- Summary, Hex, Parsed, and JSON views for a message.
- Parsed Z-Wave request/response/callback layouts, nested command classes,
  enums, flags, and length/count-driven fields.
- Parsed ZigBee EZSP frames, raw ASH link frames, and nested ZCL clusters and
  attributes.
- Store-wide Find (F3 / Shift+F3) over the current Filters result set, with
  ASCII, UTF-8, UTF-16 LE/BE, or binary hex payload predicates.

**Try without hardware**

- A bundled demo/fixture mode, usable without a gateway or controller.

## Run natively

```powershell
cd hodeco-ui
cargo run --bin hodeco-ui -- --demo
cargo run --bin hodeco-ui -- --gateway-url ws://127.0.0.1:8180/api/ws
```

`--help` prints the available options. Native defaults to
`ws://127.0.0.1:8180/api/ws`.

## Build the web bundle

The web build uses `cargo`, `wasm-bindgen`, and optionally `wasm-opt`; Trunk is
not required.

Prerequisites:

```powershell
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130
```

Install Binaryen if you want the smaller production WASM. The scripts find
`wasm-opt` on `PATH`, or accept an explicit path:

```powershell
.\scripts\build-web.ps1 -WasmOpt C:\tools\binaryen\bin\wasm-opt.exe
```

```sh
WASM_OPT=/opt/binaryen/bin/wasm-opt ./scripts/build-web.sh
```

Without `wasm-opt` the script still succeeds and prints a warning. The backend
gzip-compresses the uncompressed WASM response at runtime, but `wasm-opt`
reduces the transfer size significantly. Use a current Binaryen release; the
scripts run `wasm-opt -Oz` without experimental feature flags, because enabling
them can produce a module that browsers reject.

Build the production assets:

```powershell
cd hodeco-ui
.\scripts\build-web.ps1
```

On Linux/macOS:

```sh
cd hodeco-ui
./scripts/build-web.sh
```

The scripts produce `hodeco-ui/dist/` with the normal assets plus Brotli and
gzip sidecars:

```text
index.html
index.html.br
index.html.gz
hodeco-ui.js
hodeco-ui.js.br
hodeco-ui.js.gz
hodeco-ui_bg.wasm
hodeco-ui_bg.wasm.br
hodeco-ui_bg.wasm.gz
```

`.gz` sidecars are generated with the native gzip implementation. `.br`
sidecars are generated when the `brotli` CLI or Python's `brotli` module is
available. Raw assets are always kept as a fallback for clients that accept
neither encoding. Licenses are not copied into `dist/`; release packages include
the application and third-party notices at the package level.

Point the gateway at the output:

```yaml
httpServer:
  static:
    dir: ..\hodeco-ui\dist
    index: index.html
    spaFallback: true
    precompressed: true
```

With `precompressed: true`, `ServeDir`/`ServeFile` inspect `Accept-Encoding`
and serve the matching sidecar with `Content-Encoding: br` or `gzip`. If no
sidecar matches, the normal raw file is served. Dynamic gzip remains as a
fallback for clients that send no supported encoding header. There is no
separate UI server; `httpServer.static.dir` points directly at the built `dist/`
directory.

For local testing:

```powershell
.\scripts\serve-web.ps1
```

The page is served at `http://127.0.0.1:8080` by a plain static file server, so
pass the gateway endpoint explicitly, for example
`http://127.0.0.1:8080/?gateway_url=127.0.0.1:8180`, unless the gateway itself
serves the UI on the same origin.

Pass `-NoOptimize` / `--no-optimize` to skip `wasm-opt` during iteration, and
`-NoCompress` / `--no-compress` to skip sidecar compression.

## Startup overrides

The web build derives the WebSocket endpoint from the page origin. The query
parameters are only for remote gateways, reverse-proxy prefixes, and testing:

| URL | Effect |
| --- | --- |
| `https://gateway.example/` | `wss://gateway.example/api/ws` |
| `https://gateway.example/?gateway_url=/prefix/` | `wss://gateway.example/prefix/api/ws` |
| `http://127.0.0.1:8080/?gateway_url=192.168.1.5:8180` | `ws://192.168.1.5:8180/api/ws` |
| `http://127.0.0.1:8080/?demo=1` | bundled demo fixtures |

`?server=` and the native `--server` flag remain accepted as legacy aliases;
new links and scripts should use `--gateway-url` or `?gateway_url=`.

`startup.rs` remains the single place that parses native CLI arguments and web
query parameters. It does not introduce a UI web server.

## Layout

| Path | Purpose |
| --- | --- |
| `src/app.rs` | shell, state, gateway event reduction, actions |
| `src/app/views/` | tab views and modal dialogs |
| `src/client/` | WebSocket transport, endpoint parsing, connection state machine |
| `src/demo.rs` | in-process fixture gateway |
| `src/startup.rs` | native CLI and browser-query startup |
| `src/model.rs` | pure display helpers and message-buffer logic |
| `src/theme.rs`, `src/ui/` | theme, widgets, structured parameter editors |
| `../hodeco-codec/` | pure Z-Wave and ZigBee EZSP/ZCL decoding, hex, bundled definitions, builders, entity catalog |
| `../hodeco-schedule/` | shared calendar, solar, repeat, and jitter computation |

The UI owns colours and rendering; `hodeco-codec` owns protocol decoding and
frame construction. The UI never interprets Z-Wave or ZigBee bytes itself.

## Implementation notes

These details are here for contributors; you do not need them to use the UI.

### Transport

The UI uses WebSocket only (`ewebsock`). There is no HTTP RPC fallback and no
separate backend-for-frontend. The browser connects to
`ws(s)://<page-host>/api/ws` by default. Reconnects follow the gateway's
subscribe-then-refresh sequence described in
[WebSocket](../wiki/WebSocket.md).

### Services and parameters

Service and discovery parameter editors use the same `ParamDef` metadata as the
gateway and send explicit overrides only. The editor shows every effective
value, keeps Save disabled until the override set changes, writes only values
that differ from the definition default, and shows server-fixed fields read-only
with an explicit badge.

### Messages and filters

The Messages view keeps a bounded local list loaded through `messages.list` with
timestamp-only paging. The list uses the gateway's returned `next` bounds for
older/newer paging. Details for a message that is no longer loaded are fetched
with `messages.get`.

Table text and the inline message, run, and run-history detail panes are
selectable; `Ctrl+C` copies the selection. Row clicks still select rows, and
double-clicking a message row opens the expanded message dialog.

Filters are a separate OR-of-AND payload condition list plus state, protocol,
transport, and service metadata. **Options...** adds ANDed search-only
conditions without hiding rows. Filters are evaluated by the gateway over the
in-memory store; the UI sends protocol-neutral `PayloadExpr` trees of masked
byte leaves.

- Z-Wave frame conditions are prepared by the UI and emitted as OR-of-AND
  conditions: one frame kind per condition, coalesced contiguous leaves, and no
  wildcard padding. Frame layouts derive from `zwave.json` serial command
  fields; command-class payloads support typed fixed-width fields with
  kind-specific enum, flag, boolean, and numeric editors, with raw command data
  as the extra tail. Only checked fields participate in the match. The node field
  offers known nodes from `entities.json`, and classes without named commands use
  a numeric command ID plus raw parameters.
- The API schema has no protocol-specific fields, and the gateway does not
  decode protocol frames for filtering.
- Search is a store-wide Find (`limit: 1`) over the current Filters result set,
  not a list filter. WebSocket notifications are resolved by ID in debounced
  batches and merged into the local list.
- The table scrolls horizontally when needed, supports keyboard row navigation,
  and persists column widths through eframe's egui-memory persistence
  (`localStorage` on the web).

### Message builder

The Send message builder selects a service first; its protocol determines the
editor. Every protocol gets a universal `Raw message` operation. Z-Wave adds
`Raw Serial API frame` and `ZW_SendData (broadcast)` from `hodeco-codec`, plus
the command catalog from `zwave.json`. Command classes and their commands are
JSON-driven. The command-class picker is filterable; the command picker is a
normal dropdown.

- Commands with fields use typed controls, including packed groups, enums,
  flags, and signed or unsigned variable-width integers. Numeric values are not
  left as hex unless the wire layout is a bitmask, key, repeated list, or
  genuinely raw data.
- Classes without named commands accept raw command bytes; the selected class
  byte is added automatically.
- Each serial command's `builder` field selects its editor. Fielded commands
  render typed controls for nodes, numbers, enums, flags, booleans, and hex
  data; lengths and counts are computed automatically.
- `ZW_SendData` and `ZW_SendDataMulti` read TX options from the serial command's
  `txOptions` values (`ACK`, `LOW_POWER`, `AUTO_ROUTE`, `NO_ROUTE`, `EXPLORE`);
  ACK and `AUTO_ROUTE` are selected by default.
- The per-send TTL uses an **Override TTL** checkbox with the same
  integer-and-unit editor as service parameters; unchecked inherits the
  service's `outgoingMaxTTL` (10 s by default); `0 s` disables expiry.

ZigBee adds EZSP operations from `zigbee.json`: `version`, `networkInit`,
`networkState`, `getNetworkParameters`, `formNetwork`, `permitJoining`,
`leaveNetwork`, `getEui64`, `getNodeId`, `getChildData`, `sendUnicast`,
`sendBroadcast`, `sendMulticast`, configuration/security commands, and
decode-only callbacks. Fields use typed controls with little-endian EZSP
encoding; the gateway adds ASH framing and ACK handling. Send commands also
compose a ZCL frame: cluster/command pickers, general ZCL commands,
direction-aware request/response fields, frame-control direction and
manufacturer options, typed payload fields, and raw bytes. A known-device picker
resolves NWK addresses and endpoints through `entities.json`. Fielded requests
can also be restored from a decoded frame.

### Parsed views

- Z-Wave decoding is driven by bundled JSON definitions: request, response, and
  callback layouts; nested command classes; enums and flags; length/count-driven
  fields; and explicit truncation issues.
- ZigBee decoding covers canonical EZSP frames, raw ASH link frames, and nested
  ZCL clusters/attributes. Known sender/destination NWK addresses are labelled
  from `entities.json`.
- A ZigBee payload condition can match the command, network address, group,
  cluster, endpoint, other fixed EZSP fields, ZCL frame control, general or
  cluster ZCL commands, direction-aware typed ZCL fields, raw EZSP parameters,
  and raw ZCL payloads. Field availability, labels, and offsets come from
  semantic `filterSlot`/`role` metadata in `zigbee.json`; a Request/Response
  switch selects the EZSP field layout, and the selected command description is
  shown beneath the picker. Group ID matching covers multicast and
  group-addressed incoming frames. Fields that the selected EZSP command does
  not carry are disabled and cleared.

## Current limitations and deferred items

Configuration and service parameters are editable. `config.update` validates
and atomically writes the core config, persists only, and never restarts the
gateway automatically. Restart-scoped config sections are reported until the
gateway is restarted. Services, tasks, schedules, and commands are managed
separately: their RPC methods apply immediately and write the relevant catalog
file without a restart. `services.params` stores explicit overrides and restarts
only the affected service.

Deferred:

- Global `protocolDefaults` / `transportDefaults` config sections. The
  definition-time `ParamDef` defaults remain compiled in; service `params` are
  overrides.
- Codec data is bundled, with optional runtime overrides: `zwave.json`,
  `zigbee.json`, and `entities.json` are read next to the native executable or
  web bundle when present. Invalid files fall back to the embedded copies.
  Linux packages treat the installed `site/entities.json` as a conffile, so
  device-label edits survive upgrades; `zwave.json` and `zigbee.json` remain
  package data.

## Tests and gallery

```powershell
cd ..
cargo test -p hodeco-codec
cd hodeco-ui
cargo test --lib
cargo run --example gallery
```

The gallery does not connect to a gateway. It exercises the shared widgets,
tables, parameter editors, dialogs, toasts, hex dump, and parsed decoder.
