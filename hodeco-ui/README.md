# hodeco-ui

`hodeco-ui` is the egui-based admin and diagnostics frontend for the
[`hodeco`](../README.md) gateway. It is a separate Cargo workspace under
`hodeco-ui/` and depends on `hodeco-contract` for the wire protocol.

Project documentation and published builds are in the [wiki](../../wiki) and
[releases](../../releases). Maintainer release notes are in
[`RELEASING.md`](../RELEASING.md).

Current build includes:

- Native and WebAssembly builds from the same code.
- WebSocket-only UI transport (`ewebsock`), with no HTTP RPC fallback and no
  separate backend-for-frontend.
- Same-origin web default: the browser connects to
  `ws(s)://<page-host>/api/ws`.
- Bundled demo/fixture mode, usable without a gateway.
- App shell, top-level tabs, theme switch, banners, toasts, and reconnect
  handling.
- Services (Services/Discovery), Messages, Tasks (Tasks/Schedules/Commands/
  Runs), Configuration, Server, and Console views. Service and discovery
  parameter editors use the same `ParamDef` metadata as the gateway and send
  explicit overrides only. The task list mirrors the Services table style and
  uses text row actions.
- Store-wide Find (F3 / Shift+F3) over the current Filters result set, with
  ASCII, UTF-8, UTF-16 LE/BE, or Binary hex payload predicates. Filters are a
  separate OR-of-AND payload condition list plus state, protocol, transport,
  and service metadata; `Options...` adds ANDed search-only conditions without
  hiding rows. The message list is bounded and uses the gateway's returned
  `next` bounds for older/newer paging while the gateway evaluates raw masked
  payload expressions. Inline context and the shared Summary / Hex / Parsed /
  JSON viewer remain; details for a message that is no longer loaded are fetched
  with `messages.get`.
- Send message builder: select a service; its protocol determines the editor.
  Every protocol gets a universal `Raw message` operation. Z-Wave adds
  `Raw Serial API frame` and
  `ZW_SendData (broadcast)` from `hodeco-codec`, plus the command catalog from
  `zwave.json` (`ZW_Version`, `ZW_SendData`, `ZW_SendDataMulti`, and the rest).
  Command classes and their commands are JSON-driven. The command-class picker
  is filterable; the command picker is a normal dropdown. Commands with fields
  use typed controls, including packed groups, enums, flags, and signed or
  unsigned variable-width integers; numeric values are not left as hex unless
  the wire layout is a bitmask, key, repeated list, or genuinely raw data.
  Each serial command's `builder` field selects its editor. Fielded commands
  render typed controls for nodes, numbers, enums, flags, booleans, and hex
  data; lengths and counts are computed automatically. SendData and
  SendDataMulti read their TX options from the serial command's `txOptions`
  values (ACK, LOW_POWER, AUTO_ROUTE, NO_ROUTE, EXPLORE). The per-send TTL uses
  an **Override TTL** checkbox with the same integer-and-unit editor as service
  parameters; unchecked inherits the service's `outgoingMaxTTL` (10 s by
  default), then the protocol default. Backed by the pure `hodeco-codec` crate.
- Parsed Z-Wave decoding driven by bundled JSON definitions: request,
  response, and callback layouts; nested command classes; enums and flags;
  length/count-driven fields; and explicit truncation issues.
- A component gallery example for shared widgets and editors.

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
| `../hodeco-codec/` | pure Z-Wave/Parsed decoding, hex, bundled definitions, builder, entity catalog |

The UI owns colours and rendering; `hodeco-codec` owns protocol decoding and
frame construction. The UI never interprets Z-Wave bytes itself.

## Native

```powershell
cd hodeco-ui
cargo run --bin hodeco-ui -- --demo
cargo run --bin hodeco-ui -- --server ws://127.0.0.1:8180/api/ws
```

`--help` prints the available options. Native defaults to
`ws://127.0.0.1:8180/api/ws`.

## Web build

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

Precompressed serving is enabled by default in the gateway:

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
fallback for clients that send no supported encoding header.

There is no separate UI server; `httpServer.static.dir` points directly at the
built `dist/` directory.

For local testing:

```powershell
.\scripts\serve-web.ps1
```

The page is served at `http://127.0.0.1:8080` and connects directly to the
gateway WebSocket. The UI is served from the same origin as `/api/ws`, so no
endpoint configuration is required.

Pass `-NoOptimize` / `--no-optimize` to skip `wasm-opt` during iteration, and
`-NoCompress` / `--no-compress` to skip sidecar compression.

## Startup overrides

The web build derives the WebSocket endpoint from the page origin. The query
parameters are only for remote gateways, reverse-proxy prefixes, and testing:

| URL | Effect |
| --- | --- |
| `https://gateway.example/` | `wss://gateway.example/api/ws` |
| `https://gateway.example/?server=/prefix/` | `wss://gateway.example/prefix/api/ws` |
| `http://127.0.0.1:8080/?server=192.168.1.5:8180` | `ws://192.168.1.5:8180/api/ws` |
| `http://127.0.0.1:8080/?demo=1` | bundled demo fixtures |

`startup.rs` remains the single place that parses native CLI arguments and web
query parameters. It does not introduce a UI web server.

## Current limitations and deferred items

Configuration and service parameters are editable. `config.update` validates
and atomically writes the core config, persists only, and never restarts the
gateway automatically. Restart-scoped config sections are reported until the
gateway is restarted. Services, tasks, schedules, and commands are managed
separately: their RPC methods apply immediately and write the relevant catalog
file without a restart. `services.params` stores explicit
overrides and restarts only the affected service. The parameter editor
shows every effective value, keeps Save disabled until the override set changes,
writes only values that differ from the definition default, and shows
server-fixed fields read-only with an explicit badge.

Deferred:

- Global `protocolDefaults` / `transportDefaults` config sections. The
  definition-time `ParamDef` defaults remain compiled in; service `params` are
  overrides.
- Codec data is bundled, with optional runtime overrides: `zwave.json` and
  `entities.json` are read next to the native executable or web bundle when
  present. Invalid files fall back to the embedded copies.

The Messages view keeps a bounded local list loaded through `messages.list`
with timestamp-only paging. Filters are evaluated by the gateway over the
in-memory store; the UI sends protocol-neutral `PayloadExpr` trees of masked
byte leaves. Z-Wave frame conditions are prepared by the UI and emitted as
OR-of-AND conditions: one frame kind per condition, coalesced contiguous
leaves, and no wildcard padding. The API schema has no protocol-specific fields
and the gateway does not decode protocol frames for filtering. Search is a
store-wide Find (`limit: 1`) over the current Filters result set, not a list
filter. WebSocket notifications are resolved by ID in debounced batches and
merged into the local list. The table scrolls horizontally when needed,
supports keyboard row navigation, and persists column widths through eframe's
egui-memory persistence (`localStorage` on the web).

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
