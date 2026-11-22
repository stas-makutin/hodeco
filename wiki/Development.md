# Development

Working on hodeco is mostly ordinary Rust: one Cargo workspace for the backend,
contract, codec, and schedule engine; a second workspace for the egui UI. The
interesting part is the split between them — protocol types, decoding, and
scheduling are shared, while transport and rendering stay on their respective
sides.

If you are new to the code, start with `cargo test --workspace`, then run the
backend and the UI separately. The UI's demo mode gives you a populated app
without a gateway or a serial controller.

## Where to change things

| If you are changing… | Start in |
| --- | --- |
| A wire type, method name, error, or config field | `hodeco-contract/` |
| Z-Wave decoding, frame building, or definitions | `hodeco-codec/` |
| HTTP routes, RPC handling, WebSocket behavior | `src/http/` |
| Serial services, discovery, message storage, logging | `src/services/`, `src/messages/`, `src/logging/` |
| Tasks, schedules, commands, run history, hooks | `src/automation/` |
| Calendar, solar, repeat, or jitter math | `hodeco-schedule/` |
| A UI screen or widget | `hodeco-ui/src/app/views/`, `hodeco-ui/src/ui/` |

## Workspace layout

| Path | Purpose |
| --- | --- |
| `src/` | Backend server, HTTP/WebSocket API, services, logging, persistence. |
| `tests/` | Backend integration tests. |
| `hodeco-contract/` | Shared wire types, DTOs, method names, errors, config schema, and parameter metadata. |
| `hodeco-codec/` | Pure Z-Wave/frame codec, hex helpers, builder, and static catalog shared with the UI. |
| `hodeco-schedule/` | Calendar, solar, repeat, and jitter computation shared with the UI. |
| `hodeco-ui/` | Separate workspace for the egui admin/diagnostics frontend. |
| `wiki/` | Source for the GitHub wiki. |
| `packaging/` | nFPM package definition used by the release workflow. |

`hodeco-codec/data/zwave.json` is the Z-Wave catalog. It covers the Serial API
function IDs and the known command-class IDs. Common commands have typed
field-level layouts, including packed bit groups, enums, flags, and signed or
unsigned integers whose width comes from `lengthFrom` or a fixed `length`;
less common layouts fall back to named raw bytes until they are filled in. See
[Codec Data Files](Data-Files) for both JSON formats.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Cross-check the backend for Linux:

```sh
cargo check --target x86_64-unknown-linux-gnu --all-targets
```

Check the shared contract as a WASM-compatible dependency:

```sh
cargo check -p hodeco-contract --target wasm32-unknown-unknown
```

### Real controller test

The ignored Z-Wave test uses a real serial controller. Set
`HODECO_SERIAL_PORT` (default `COM4`):

```powershell
$env:HODECO_SERIAL_PORT = "COM4"
cargo test --test real_controller -- --ignored --nocapture
```

## Frontend

The UI is a separate workspace. Build and run it with:

```powershell
cd hodeco-ui
cargo run --bin hodeco-ui -- --demo
cargo run --bin hodeco-ui -- --server ws://127.0.0.1:8180/api/ws
```

Build the web assets:

```powershell
.\scripts\build-web.ps1
```

```sh
./scripts/build-web.sh
```

The scripts need `wasm-bindgen` matching `hodeco-ui/Cargo.lock` and optionally
`wasm-opt`. The result is raw assets plus Brotli and gzip sidecars; licenses are
added at the package level, not inside `dist/`. See the
[UI README](../blob/HEAD/hodeco-ui/README.md) for details.

## Licenses

Third-party licenses and attribution notices are listed in
[`THIRD-PARTY-LICENSES.md`](../blob/HEAD/THIRD-PARTY-LICENSES.md) for the backend
and
[`hodeco-ui/THIRD-PARTY-LICENSES.md`](../blob/HEAD/hodeco-ui/THIRD-PARTY-LICENSES.md)
for the frontend. Regenerate both after a dependency change:

```powershell
.\scripts\generate-licenses.ps1
```

```sh
./scripts/generate-licenses.sh
```

Both scripts require `cargo-about`. The allow-list in `deny.toml` is verified
with:

```sh
cargo deny check licenses
cargo deny --manifest-path hodeco-ui/Cargo.toml check licenses
```

## CI and releases

Ordinary branch and pull-request pushes do not run CI. The release workflow runs
formatting, clippy, tests, the WASM contract check, and cross-platform builds
when a `v*` tag is pushed. It can also be run manually from the Actions tab
without publishing.

See [`RELEASING.md`](../blob/HEAD/RELEASING.md) for the target list, artifact
layout, and the wiki token setup.

## Wiki

Wiki pages are edited in `wiki/` and synced to the GitHub wiki by
`.github/workflows/wiki.yml` on pushes to `master` (or `main`) that touch
`wiki/`. `Home.md` is required; `_Sidebar.md` and `_Footer.md` are wiki
navigation helpers.

The workflow treats `wiki/` as the source of truth and replaces the wiki
contents, so edit pages in `wiki/` rather than in the GitHub wiki UI.
