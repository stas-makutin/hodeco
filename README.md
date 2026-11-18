# hodeco

**Share the home devices on your serial controllers with everything else on your
network.**

hodeco turns a serial controller into a shared gateway for your local network.
Z-Wave is supported today; ZigBee is planned. One process owns the controller,
and the bundled web UI, your scripts, and your own automation all use the same
typed JSON-RPC API, live WebSocket stream, and searchable message history.

That means no more fighting over the serial port: connect the controller once,
then let every local client share the same devices, state, and history. hodeco
is built to run on a small always-on machine — a Raspberry Pi, a mini-PC, a
Windows box — and to stay out of the way: one binary, a small set of YAML files,
no external database, no cloud account.

> **Trusted networks only.** Authentication and TLS are not implemented, and the
> default bind address is `127.0.0.1`. If you expose it beyond your LAN, put it
> behind a reverse proxy that provides both.

**Contents:** [What you get](#what-you-get) ·
[Supported controllers](#supported-controllers) · [Quick start](#quick-start) ·
[How it fits together](#how-it-fits-together) · [API tour](#a-tour-of-the-api) ·
[Message builder](#message-builder) · [Configuration](#configuration) ·
[Running as a service](#running-as-a-service) · [Build and test](#build-and-test) ·
[Releases](#releases) · [Changelog](CHANGELOG.md) · [Documentation](#documentation) ·
[License](#license)

## What you get

- **One owner for the serial controller.** The gateway holds the serial port, so
  the UI, your scripts, and your own automation share one device state and one
  history instead of competing for the port.
- **One process, one deployment.** The backend serves the API, the WebSocket
  stream, and the compiled WASM UI. No separate frontend server, database, or
  container stack.
- **Typed end to end.** `hodeco-contract` is the shared source of truth for
  methods, payloads, errors, and the configuration schema. The backend and the
  WASM frontend compile against the same types.
- **A real event stream.** Subscribe to messages, discovery, and service state.
  A slow consumer is disconnected with code `1013` instead of silently losing
  events, so clients know when to resynchronize.
- **Built for diagnostics.** Page through message history, filter by time,
  metadata, IDs, or masked payload expressions, inspect parsed Z-Wave frames,
  and build outgoing frames with typed controls, packed bit fields, signed or
  unsigned integer widths, TX options, and a per-send TTL override.
- **Automation included.** Tasks run commands and send messages on schedules,
  messages, HTTP hooks, or manual triggers; next rules chain tasks, and every run
  is recorded in per-task history. Commands and schedules are reusable catalogs.
- **Operations included.** Rotating logs with optional ZIP archives, file-backed
  message and task persistence, a Windows service host, and a systemd installer.
- **Portable releases.** Windows x64/ARM64, Linux x64/ARM64/ARMv7, and fully
  static musl builds. Windows builds use the static CRT.

## Supported controllers

| Protocol | Transport | Status |
| --- | --- | --- |
| Z-Wave | serial | Supported |
| ZigBee | serial | Planned |

Each controller is one `services` entry in the config. The gateway owns the
serial port; clients share the devices through the API. New protocols are added
behind the same contract, so the API shape stays consistent as support grows.

## Quick start

### Run a release

Download the archive for your platform from [Releases](../../releases), extract
it, and start the service:

```powershell
# Windows
Expand-Archive hodeco-<version>-windows-x86_64.zip -DestinationPath hodeco
cd hodeco
.\hodeco.exe run
```

```sh
# Linux portable
tar -xzf hodeco-<version>-linux-x86_64.tar.gz
cd hodeco-<version>-linux-x86_64
./hodeco run --config config.yaml
```

Open <http://127.0.0.1:8180/> for the web UI. The Windows archive works with the
platform defaults; the Linux archive includes a portable `config.yaml` so the
working directory, logs, and `site/` assets stay next to the binary.

### Build from source

You need Rust 1.95 or newer. On Windows, install the Visual Studio C++ build
tools and Windows SDK; the build embeds the Event Log message table.

```sh
cargo build --workspace
cargo run -- run
```

The backend is checked for Linux and the shared contract is checked for WASM:

```sh
cargo check --target x86_64-unknown-linux-gnu --all-targets
cargo check -p hodeco-contract --target wasm32-unknown-unknown
```

### Build the web UI

The UI is a separate workspace. Install the `wasm32` target and a matching
`wasm-bindgen` CLI, then run the platform script:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130
```

```powershell
cd hodeco-ui
.\scripts\build-web.ps1
```

```sh
cd hodeco-ui
./scripts/build-web.sh
```

The output is `hodeco-ui/dist`: raw assets, Brotli sidecars, and gzip sidecars.
Point `httpServer.static.dir` at that directory. Precompressed serving is on by
default; clients that do not accept Brotli or gzip still receive the raw files.

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
             hodeco-contract ──┘   │   └── operational log + message log
             (wire types,          │
              methods, schema)     └── serial ── controller
                                                (Z-Wave now,
                                                 ZigBee planned)
```

| Crate | What it does |
| --- | --- |
| `hodeco` | Backend server: HTTP/WebSocket API, service lifecycle, logging, message store, and the serial services (Z-Wave today, ZigBee planned). |
| `hodeco-contract` | Shared wire types, method names, errors, configuration schema, parameter metadata, and the static Z-Wave/serial catalog. No Tokio or transport dependencies; builds for WASM. |
| `hodeco-ui` | egui admin and diagnostics frontend. Native and browser/WASM builds from the same code. |
| `hodeco-codec` | Pure Z-Wave/Parsed decoding, hex helpers, frame builder, command definitions, and entity catalog shared with the UI. |

## A tour of the API

The gateway exposes four routes:

| Route | Purpose |
| --- | --- |
| `POST /api/rpc` | JSON-RPC-style request/response. |
| `GET /api/ws` | WebSocket for RPC calls and live notifications. |
| `GET /api/health` | Health check. |
| `GET /*` | Static frontend assets. |

A request is intentionally boring:

```sh
curl -s http://127.0.0.1:8180/api/rpc \
  -H 'content-type: application/json' \
  -d '{"id":"1","method":"server.info","params":{}}'
```

The envelope is JSON-RPC-flavored but minimal: `id` is a string, `method` is a
known method name, and `params` is an object. The `jsonrpc` field is not used.

The method surface covers server and config (`server.info`, `config.get`,
`config.update`), metadata (`protocols.*`, `transports.list`), discovery
(`discovery.start`, `discovery.get`, `discovery.cancel`), services
(`services.list`, `services.add`, `services.remove`, `services.alias`,
`services.status`, `services.params`, `services.send`), messages
(`messages.list`, `messages.get`), and events (`events.subscribe`,
`events.unsubscribe`).

`messages.list` is the workhorse: timestamp-bounded paging, metadata and ID
filters, and protocol-neutral masked `PayloadExpr` trees evaluated by the
gateway, not the browser.

On the WebSocket, no events are delivered until the client subscribes:

```json
{"id":"1","method":"events.subscribe","params":{"topics":["messages","services"]}}
```

Topics are `messages`, `discovery`, and `services`. Each connection has a bounded
outgoing queue; a slow consumer, or a lagging server-side event stream, closes
the connection with `1013`. Reconnect, resubscribe, and resynchronize with the
list methods.

The included UI is an egui app compiled to WASM. It covers services, discovery,
message history, tasks/schedules/commands/runs, configuration, server details,
and a console, plus a protocol builder and a demo/fixture mode that works without
a gateway. The browser
connects to `ws(s)://<page-host>/api/ws` by default; use `?server=` only for a
remote gateway.

`services.send` accepts an optional `ttl`. Omit it to inherit the service's
`outgoingMaxTTL` (10 s by default), then the protocol default.

## Message builder

The UI's **Send message** dialog is driven by
[`hodeco-codec/data/zwave.json`](hodeco-codec/data/zwave.json):

- Operations are searchable and include a universal raw message, a raw Serial
  API frame, `ZW_SendData`/`ZW_SendDataMulti`, broadcast, and the catalog's
  serial commands. Command classes are searchable; commands for the selected
  class use a normal dropdown.
- Request fields use typed controls. Enums, flags, packed bit groups, nodes,
  durations, text, and signed or unsigned integers come from the definition;
  `int` and `uint` widths come from a `lengthFrom` enum mapping or a fixed
  `length`. `hex` is reserved for bitmasks, keys, repeated lists, offsets, and
  genuinely raw payloads.
- `ZW_SendData` and `ZW_SendDataMulti` read their TX options from the serial
  command's `txOptions` values: `ACK`, `LOW_POWER`, `AUTO_ROUTE`, `NO_ROUTE`,
  and `EXPLORE`. Broadcast keeps its fixed recommended options.
- The per-send TTL uses the same integer-and-unit editor as the service
  parameter editor. Unchecked inherits the service override and then the 10 s
  protocol default; checked sends an explicit value.
- Lengths and counts are computed from the entered data, and the command ID,
  frame length, and checksum are added automatically. Commands without a
  documented layout still accept optional raw parameter bytes.

See [Codec Data Files](wiki/Data-Files.md) for the full schema.

## Configuration

The configuration file is optional. These are the platform defaults:

| Item | Windows | Linux |
| --- | --- | --- |
| Config | `<exe_dir>\config.yaml` | `/etc/hodeco/config.yaml` |
| Working directory | `<exe_dir>` | `/var/lib/hodeco` |
| Log file | `<exe_dir>\log\hodeco.log` | `/var/log/hodeco/hodeco.log` |
| Message log | `<exe_dir>\messages.log` | `/var/lib/hodeco/messages.log` |
| Static assets | `<exe_dir>\site` | `/usr/share/hodeco/site` |

Configuration is split into five sibling files. `config.yaml` holds the core
settings and points at the catalogs; the catalog files live next to it.

```yaml
# config.yaml
workingDir: /var/lib/hodeco

log:
  dir: /var/log/hodeco
  file: hodeco.log
  dirMode: "0755"
  fileMode: "0644"
  maxSize: 100 MiB
  maxAge: 1 d
  backups: 5
  backupDays: 14
  archive: none       # none or zip

httpServer:
  bind: 127.0.0.1
  port: 8180
  maxConnections: 1000
  readTimeout: 5 s
  readHeaderTimeout: 5 s
  writeTimeout: 5 s
  idleTimeout: 2 m
  maxHeaderBytes: 8192
  static:
    dir: /usr/share/hodeco/site
    index: index.html
    spaFallback: true
    precompressed: true

messageLog:
  maxSize: 10 MiB
  file: /var/lib/hodeco/messages.log
  fileMode: "0644"
  autoPersist: 60 s
  compactionFactor: 2.0
  compactInterval: 30 d
  onCorrupt: ignore  # ignore or fail

files:
  services: services.yaml
  tasks: tasks.yaml
  schedules: schedules.yaml
  commands: commands.yaml

taskLog:
  dir: tasks
  maxSize: 100 KiB
  maxRows: 1000
  fileMode: "0640"
  compactionFactor: 2.0
  compactInterval: 30 d
  onCorrupt: ignore

taskEngine:
  maxTriggerPayloadSize: 16 KiB
  maxConcurrentRuns: 32
  maxChainDepth: 16
  maxFanOut: 16
```

```yaml
# services.yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0
    params:
      baudRate: 115200
      openAttemptsInterval: 3 s
      outgoingMaxTTL: 10 s
```

```yaml
# commands.yaml
commands:
  - id: meter
    name: Meter poll
    executable: /usr/local/bin/meter-poll
    args: ["--json"]
    timeout: 10s
    description: Read the meter value
```

```yaml
# schedules.yaml
location: [52.52, 13.405, 34]
timezone: Europe/Berlin

schedules:
  - id: before-sunset
    name: Before sunset
    on:
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
```

`outgoingMaxTTL` bounds how long a queued outgoing message may wait; `0 s`
disables the TTL. The builder's **Override TTL** sends an explicit per-message
value without changing the service configuration.

Relative core paths are resolved after the configured `workingDir` is created
and entered. Catalog paths in `files` are resolved against the directory that
contains `config.yaml`. Unknown fields are rejected. Service aliases and keys,
task/schedule/command IDs, and trigger/action IDs must be unique. Outside
changes to any file require a manual restart; there is no file watcher.

The service, task, schedule, and command RPC methods rewrite the relevant file
atomically. The service account must be able to write those files for write-back
to succeed. See [`CHANGELOG.md`](CHANGELOG.md) for the 0.2.0 configuration and
message-log incompatibilities.

## Running as a service

Windows, from an elevated prompt:

```text
hodeco install
hodeco start
hodeco status
hodeco stop
hodeco uninstall
```

The Windows service runs as `LocalSystem`. An explicit `--config` path is
converted to an absolute path during installation.

Linux, with systemd:

```sh
sudo hodeco install [--config /path/to/config.yaml] [--user hodeco]
sudo hodeco start
sudo hodeco status
```

`--user` defaults to `SUDO_USER`, then `USER`. The account must exist. A warning
is printed when it is not a member of `dialout`, `uucp`, or `plugdev`.

The unit uses `LogsDirectory=hodeco` and `StateDirectory=hodeco`, which create
`/var/log/hodeco` and `/var/lib/hodeco` for the service user. `/etc/hodeco` is
root-owned and the config file is expected to be `0644`.

## Build and test

Run the local checks before sending a change:

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

A real-controller test is available as an ignored test. Without
`HODECO_SERIAL_PORT`, it uses `COM4`:

```powershell
$env:HODECO_SERIAL_PORT = "COM4"
cargo test --test real_controller -- --ignored --nocapture
```

Tagged releases are built by GitHub Actions; ordinary branch and pull-request
pushes do not run CI. See [`RELEASING.md`](RELEASING.md) for the release
checklist, target matrix, and artifact layout.

## Releases

Tagged releases ship the backend and web UI together:

| Platform | Target |
| --- | --- |
| Windows x64 / ARM64 | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` |
| Linux x64 / ARM64 / ARMv7 (glibc) | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf` |
| Linux x64 / ARM64 (static) | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` |

Linux glibc targets also produce `.deb` and `.rpm` packages. Each release
includes `SHA256SUMS` and generated release notes. See
[`RELEASING.md`](RELEASING.md) for the full artifact layout.

## Documentation

The [wiki](../../wiki) is the operator and integrator manual:

- [Getting Started](wiki/Getting-Started.md) — run a release or build from source.
- [Changelog](CHANGELOG.md) — release history and breaking changes.
- [Configuration](wiki/Configuration.md) — full config reference and paths.
- [HTTP API](wiki/HTTP-API.md) — endpoints, RPC methods, and static assets.
- [WebSocket](wiki/WebSocket.md) — subscriptions, notifications, and backpressure.
- [Logging](wiki/Logging.md) — operational logs and message persistence.
- [Releases](wiki/Releases.md) — supported targets, archive layout, and packages.
- [Development](wiki/Development.md) — workspace commands and cross-checks.
- [Codec Data Files](wiki/Data-Files.md) — `zwave.json` and `entities.json` formats.

Maintainer release instructions live in [`RELEASING.md`](RELEASING.md).

## License

Licensed under either of

- MIT license ([`LICENSE-MIT`](LICENSE-MIT))
- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))

at your option.

Third-party licenses and attribution notices are listed in
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) for the backend and
[`hodeco-ui/THIRD-PARTY-LICENSES.md`](hodeco-ui/THIRD-PARTY-LICENSES.md) for the
frontend. Release packages include both as `THIRD-PARTY-LICENSES.md` and
`THIRD-PARTY-LICENSES-UI.md`.

Regenerate both after a dependency change:

```powershell
.\scripts\generate-licenses.ps1
```

```sh
./scripts/generate-licenses.sh
```

Both scripts require `cargo-about`. The allow-list in [`deny.toml`](deny.toml) is
verified with:

```sh
cargo deny check licenses
cargo deny --manifest-path hodeco-ui/Cargo.toml check licenses
```
