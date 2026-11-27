# hodeco

**Share the home devices on your serial controllers with everything else on your
network.**

hodeco is a local gateway for Z-Wave and ZigBee serial controllers. It owns the
serial port once, keeps the device state and message history, and gives the
bundled web UI, your scripts, and your own automation the same typed JSON-RPC
API and live WebSocket stream.

No more fighting over the serial port: connect the controller once, then let
every local client share the same devices, state, and history. hodeco is built
to run on a small always-on machine — a Raspberry Pi, a mini-PC, a Windows box —
and to stay out of the way: one binary, a small set of YAML files, no external
database, no cloud account.

> **Trusted networks only.** Authentication and TLS are not implemented, and the
> default bind address is `0.0.0.0`, so the UI and API listen on every network
> interface. Set `httpServer.bind` to `127.0.0.1` to restrict access to the local
> host. If you expose it beyond your LAN, put it behind a reverse proxy that
> provides both.

**On this page:** [What hodeco does](#what-hodeco-does) ·
[Quick start](#quick-start) · [Connect a controller](#connect-a-controller) ·
[How it fits together](#how-it-fits-together) · [Use the API](#use-the-api) ·
[Send and inspect messages](#send-and-inspect-messages) ·
[Configuration](#configuration) · [Running as a service](#running-as-a-service) ·
[Build and test](#build-and-test) · [Releases](#releases) ·
[Documentation](#documentation) · [License](#license)

## New here? Pick a path

| I want to… | Start with |
| --- | --- |
| Try it quickly and see the UI | [Quick start](#quick-start) |
| Connect my Z-Wave or ZigBee controller | [Quick start](#quick-start), then [Connect a controller](#connect-a-controller) |
| Understand the terms first | [How hodeco works](wiki/Concepts.md) |
| Automate something | [Tasks](wiki/Tasks.md) and [Schedules](wiki/Schedules.md) |
| Call hodeco from a script | [HTTP API](wiki/HTTP-API.md) and [WebSocket](wiki/WebSocket.md) |
| Build it from source or contribute | [Build and test](#build-and-test) and [Development](wiki/Development.md) |

The full documentation lives in [`wiki/`](wiki/) and is published as the
[wiki](../../wiki); this README is the short front door.

## What hodeco does

hodeco has one main job and a few useful extras.

**It owns the controller.** A Z-Wave or ZigBee stick is a serial device, and
normally only one program can open its port. hodeco holds the port, then serves
the same devices to the UI, your scripts, and your automation. One service entry
per controller; every client sees the same state and history.

**It keeps a real message history.** Incoming and outgoing frames are stored in
a bounded, file-backed log. You can page through it, filter by time, metadata,
IDs, or masked payload expressions, and inspect parsed Z-Wave frames and ZigBee
EZSP/ZCL frames. The message builder can also compose outgoing frames with typed
fields, packed bit groups, integer widths, TX options, and a per-send TTL.

**It streams events.** The UI and your own clients can subscribe over a
WebSocket to messages, discovery, service state, and task runs. A slow consumer
is disconnected with code `1013` rather than silently losing events, so clients
know when to resynchronize.

**It includes automation.** Tasks run commands and send messages on schedules,
incoming messages, HTTP hooks, or manual triggers. Next rules chain tasks, a
bounded recent-run feed powers the Runs view, and opt-in per-task history keeps
runs on disk. Free-form task labels drive the task-list filter, and commands and
schedules are reusable catalogs.

**It is easy to operate.** Rotating logs with ZIP archives by default, file-backed
message and task persistence, a Windows service host, and a systemd installer are
included. Releases cover Windows x64/ARM64 with a static CRT, glibc Linux
x64/ARM64/ARMv7, and fully static musl builds for Linux x64/ARM64/ARMv6.

## Supported controllers

| Protocol | Transport | Status |
| --- | --- | --- |
| Z-Wave | serial | Supported |
| ZigBee (Ember EZSP) | serial | Supported |

Each controller is one entry in `services.yaml`. The gateway owns the serial
port; clients share the devices through the API. New protocols are added behind
the same contract, so the API shape stays consistent as support grows.

## Quick start

### Run a release

Download the archive for your platform from [Releases](../../releases), extract
it, and start the service.

Windows:

```powershell
Expand-Archive hodeco-<version>-windows-x86_64.zip -DestinationPath hodeco
cd hodeco
.\hodeco.exe run
```

Linux portable:

```sh
tar -xzf hodeco-<version>-linux-x86_64.tar.gz
cd hodeco-<version>-linux-x86_64
./hodeco run --config config.yaml
```

Then:

1. Open <http://127.0.0.1:8180/> for the web UI. From another machine on the
   network, use the host's address instead, for example
   <http://192.168.1.5:8180/>.
2. Check <http://127.0.0.1:8180/api/health> if the page does not load.
3. Add your controller in the UI, or put it in `services.yaml` and restart.

The Windows archive works with the platform defaults. The Linux archive includes
a portable `config.yaml` so the working directory, logs, and `site/` assets stay
next to the binary. The `.deb`/`.rpm` packages install a systemd service and are
covered in [Running as a service](#running-as-a-service).

### Build from source

You need Rust 1.95 or newer. On Windows, install the Visual Studio C++ build
tools and Windows SDK; the build embeds the Event Log message table.

```sh
cargo build --workspace
cargo run -- run
```

The backend is checked for Linux and the shared contract for WASM:

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

**Next:** [Getting Started](wiki/Getting-Started.md) walks through finding your
serial port, adding the first controller, and checking that frames arrive.

## Connect a controller

Each controller is one entry in `services.yaml`, next to `config.yaml`:

```yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0   # COM4 on Windows
    params:
      baudRate: 115200
  - alias: ZB
    protocol: zigbee
    transport: serial
    entry: /dev/ttyUSB1   # COM5 on Windows
    params:
      baudRate: 57600
      flowControl: none
```

The baud rates above are common defaults, not universal. Check the controller's
manual. `alias` is the short name used by the UI and by task references, so keep
it stable.

You can also add the service in the UI. Changes made through the API/UI apply
immediately. If you edit `services.yaml` yourself, restart hodeco; there is no
file watcher.

If you do not know the port name:

- **Windows:** check Device Manager under **Ports (COM & LPT)**, or run
  `Get-CimInstance Win32_SerialPort | Select-Object DeviceID, Description`.
- **Linux:** run `ls -l /dev/serial/by-id/` or `ls /dev/ttyUSB* /dev/ttyACM*`.
  Prefer the stable `/dev/serial/by-id/...` path when one exists.
- **Permissions:** the service account must be in `dialout`, `uucp`, or
  `plugdev` (whichever exists). The packages handle this; for a manual install,
  add the user and sign out and back in.

[Configuration](wiki/Configuration.md) covers the remaining service parameters,
including timeouts, flow control, reconnect interval, and `outgoingMaxTTL`.

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
                                                (Z-Wave and
                                                 ZigBee EZSP)
```

| Crate | What it does |
| --- | --- |
| `hodeco` | Backend server: HTTP/WebSocket API, service lifecycle, logging, message store, and the serial services (Z-Wave and ZigBee EZSP). |
| `hodeco-contract` | Shared wire types, method names, errors, configuration schema, parameter metadata, and the static protocol catalog. No Tokio or transport dependencies; builds for WASM. |
| `hodeco-schedule` | Pure calendar, solar, repeat, and jitter computation shared by the gateway scheduler and the UI preview/demo paths. |
| `hodeco-ui` | egui admin and diagnostics frontend. Native and browser/WASM builds from the same code. |
| `hodeco-codec` | Pure Z-Wave and ZigBee EZSP/ZCL decoding, hex helpers, frame builders, command definitions, and entity catalog shared with the UI. |

## Use the API

The gateway exposes the API, the live WebSocket, health, task hooks, and the
static frontend:

| Route | Purpose |
| --- | --- |
| `POST /api/rpc` | JSON-RPC-style request/response. |
| `GET /api/ws` | WebSocket for RPC calls and live notifications. |
| `GET /api/health` | Health check. |
| `GET/POST /api/hooks/{task}` | Starts a task's HTTP trigger. |
| `GET /*` | Static frontend assets. |

A first request is intentionally boring:

```sh
curl -s http://127.0.0.1:8180/api/rpc \
  -H 'content-type: application/json' \
  -d '{"id":"1","method":"server.info","params":{}}'
```

The envelope is JSON-RPC-flavored but minimal: `id` is a string, `method` is a
known method name, and `params` is an object. The `jsonrpc` field is not used.

The method surface is grouped by job:

| Area | Methods |
| --- | --- |
| Server and config | `server.info`, `system.restart`, `config.get`, `config.update` |
| Metadata | `protocols.*`, `transports.list` |
| Discovery | `discovery.start`, `discovery.get`, `discovery.cancel` |
| Services | `services.list`, `services.add`, `services.remove`, `services.alias`, `services.status`, `services.params`, `services.send` |
| Messages | `messages.list`, `messages.get` |
| Automation | `tasks.*`, `schedules.*`, `commands.*` |
| Events | `events.subscribe`, `events.unsubscribe` |

`messages.list` is the workhorse: timestamp-bounded paging, metadata and ID
filters, and protocol-neutral masked `PayloadExpr` trees evaluated by the
gateway, not the browser.

On the WebSocket, no events are delivered until the client subscribes:

```json
{"id":"1","method":"events.subscribe","params":{"topics":["messages","services"]}}
```

Topics are `messages`, `discovery`, `services`, and `tasks`. Each connection has
a bounded outgoing queue; a slow consumer, or a lagging server-side event stream,
closes the connection with `1013`. Reconnect, resubscribe, and resynchronize with
the list methods.

The included UI is an egui app compiled to WASM. It covers services, discovery,
message history, tasks/schedules/commands/runs, configuration, server details,
and a console, plus a protocol builder and a demo/fixture mode that works without
a gateway. The browser connects to `ws(s)://<page-host>/api/ws` by default; use
`?gateway_url=` for a remote gateway or a reverse-proxy prefix.

See [HTTP API](wiki/HTTP-API.md) for recipes and [WebSocket](wiki/WebSocket.md)
for the event stream and reconnect sequence.

## Send and inspect messages

The **Messages** view shows the history and a decoded view for each frame. The
**Send message** dialog is driven by
[`hodeco-codec/data/zwave.json`](hodeco-codec/data/zwave.json) and
[`hodeco-codec/data/zigbee.json`](hodeco-codec/data/zigbee.json).

- Operations are searchable. Every protocol gets a universal raw message; Z-Wave
  also has a raw Serial API frame and a broadcast shortcut, and both protocols
  list their catalog entries.
- Request fields use typed controls: enums, flags, packed bit groups, nodes,
  durations, text, and signed or unsigned integers. Lengths and counts are
  computed where the definition allows it.
- Z-Wave `ZW_SendData` and `ZW_SendDataMulti` read TX options from the serial
  command definition. Broadcast keeps its fixed recommended options.
- The per-send TTL overrides the service's `outgoingMaxTTL` for one message
  without changing the service configuration.
- ZigBee operations cover the EZSP coordinator commands in `zigbee.json`. The
  gateway adds ASH framing, CRC, escaping, and ACK handling; the builder shows
  the canonical EZSP frame and the decoded ZCL payload where applicable.
- Commands without a documented layout still accept optional raw parameter
  bytes, so the builder never blocks you on a missing definition.

If you need to extend the builder or label devices, see
[Codec Data Files](wiki/Data-Files.md) for the JSON schema.

## Configuration

Configuration is optional. With no files at all, hodeco starts with platform
defaults. These are the paths to know:

| Item | Windows | Linux |
| --- | --- | --- |
| Config | `<exe_dir>\config.yaml` | `/etc/hodeco/config.yaml` |
| Working directory | `<exe_dir>` | `/var/lib/hodeco` |
| Log file | `<exe_dir>\log\hodeco.log` | `/var/log/hodeco/hodeco.log` |
| Message log | `<exe_dir>\messages.log` | `/var/lib/hodeco/messages.log` |
| Static assets | `<exe_dir>\site` | `/usr/share/hodeco/site` |

The configuration is split into five sibling files. `config.yaml` holds the core
settings and points at the catalogs.

| File | Purpose |
| --- | --- |
| `config.yaml` | Server, logging, message history, file paths, task-engine limits. |
| `services.yaml` | One entry per controller. |
| `commands.yaml` | External programs that tasks can run. |
| `schedules.yaml` | Shared timezone/location and reusable schedules. |
| `tasks.yaml` | Triggers, actions, next rules, and history settings. |

A minimal `services.yaml` is enough to get started:

```yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0
    params:
      baudRate: 115200
```

Relative core paths are resolved after the configured `workingDir` is created
and entered. Catalog paths in `files` are resolved against the directory that
contains `config.yaml`. Unknown fields are rejected, and service aliases and
keys, task/schedule/command IDs, and trigger/action IDs must be unique. Changes
made by editing the files directly require a manual restart; there is no file
watcher.

[Configuration](wiki/Configuration.md) has the full reference, platform
defaults, service parameters, and the notes on write-back and permissions.

## Running as a service

The `.deb` and `.rpm` packages install the systemd unit and manage the account
and permissions:

```sh
sudo apt install ./hodeco_0.3.2_amd64.deb
# or
sudo dnf install ./hodeco-0.3.2-1.x86_64.rpm
```

Installation creates the `hodeco` system user and group, adds the account to
`dialout`, `uucp`, and `plugdev` when those groups exist, and creates
`/etc/hodeco`, `/var/lib/hodeco`, and `/var/log/hodeco`. The package enables
`hodeco.service` and starts it on a fresh install. On upgrade it stops the
running service before replacing files and starts it again only if it had been
running. `/etc/hodeco/config.yaml` is a package configuration file, and
`/etc/hodeco` is group-writable by `hodeco` so configuration and catalog
write-back through the API works. The device-label catalog
`site/entities.json` is also a configuration file, so local label edits survive
package upgrades. While the package owns the unit, `hodeco install` and
`hodeco uninstall` leave it alone; use the package manager instead.

```sh
sudo systemctl status hodeco
sudo systemctl restart hodeco
```

For a portable archive, register the service manually.

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
is printed when it is not a member of `dialout`, `uucp`, or `plugdev`. The unit
uses `LogsDirectory=hodeco` and `StateDirectory=hodeco`, which create
`/var/log/hodeco` and `/var/lib/hodeco` for the service user.

See [Getting Started](wiki/Getting-Started.md) for the full service-install
notes and the differences between package and manual installs.

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

The ZigBee probe uses `HODECO_ZIGBEE_PORT`, defaulting to `COM5`:

```powershell
$env:HODECO_ZIGBEE_PORT = "COM5"
cargo test --test real_zigbee -- --ignored --nocapture
```

Tagged releases are built by GitHub Actions; ordinary branch and pull-request
pushes do not run CI. See [`RELEASING.md`](RELEASING.md) for the release
checklist, target matrix, and artifact layout.

## Releases

Tagged releases ship the backend and web UI together:

| Platform | Target |
| --- | --- |
| Windows x64 / ARM64 (static CRT) | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` |
| Linux x64 / ARM64 / ARMv7 (glibc) | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf` |
| Linux x64 / ARM64 (static) | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` |
| Linux ARMv6 (static, Raspberry Pi 1/Zero and 32-bit Raspberry Pi OS) | `arm-unknown-linux-musleabihf` |

Windows builds link the MSVC C runtime statically, so the ZIPs do not require
the Visual C++ Redistributable. Musl builds are fully static and do not depend
on the host glibc. Linux glibc targets also produce `.deb` and `.rpm` packages.
Each release includes `SHA256SUMS` and uses the matching `CHANGELOG.md` section
as its release notes. [Releases](wiki/Releases.md) explains which archive to
pick, what is inside it, and how upgrades work.

## Documentation

The [wiki](../../wiki) is the operator and integrator manual. The source lives
in [`wiki/`](wiki/) and is published by a workflow.

**Learn**

- [How hodeco works](wiki/Concepts.md) — vocabulary and mental model.
- [Getting Started](wiki/Getting-Started.md) — release, package, or source, plus a first-run check.
- [Configuration](wiki/Configuration.md) — full config reference and platform paths.

**Use**

- [Tasks](wiki/Tasks.md) — triggers, actions, next rules, hooks, and run history.
- [Schedules](wiki/Schedules.md) — recurrence, solar events, repeats, and previews.
- [Persistence](wiki/Persistence.md) — message and task-history storage.
- [Logging](wiki/Logging.md) — operational logs and message persistence.

**Integrate**

- [HTTP API](wiki/HTTP-API.md) — endpoints, RPC methods, and recipes.
- [WebSocket](wiki/WebSocket.md) — subscriptions, notifications, and backpressure.
- [Codec Data Files](wiki/Data-Files.md) — `zwave.json`, `zigbee.json`, and `entities.json` formats.

**Project**

- [Releases](wiki/Releases.md) — supported targets, archive layout, and packages.
- [Development](wiki/Development.md) — workspace commands and cross-checks.
- [Changelog](CHANGELOG.md) — release history and breaking changes.
- [RELEASING.md](RELEASING.md) — maintainer release instructions.

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
