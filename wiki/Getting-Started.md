# Getting Started

hodeco shares the home devices attached to your serial controllers with the rest
of your local network. Z-Wave is supported today; ZigBee is planned.

You can run it from a release archive, install it as a Linux package, or build
it from source. All three end at the same place: a gateway on
`http://127.0.0.1:8180/` with the web UI served by the same process.

## Choose a path

| Path | Best for |
| --- | --- |
| Release archive | A quick look, a portable install, or Windows. |
| `.deb` / `.rpm` | A system-wide Linux install with a systemd service. |
| Source | Development, a custom build, or a platform we do not package. |

## From a release

Download the archive for your platform from [Releases](../releases) and extract
it. The archive contains the backend, the built web UI in `site/`, and the
license files.

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

`run` starts the gateway in the foreground. Open
<http://127.0.0.1:8180/> for the web UI.

On Windows the default static directory is `<exe_dir>\site`, so the bundled UI
is used automatically. Linux portable archives include a `config.yaml` that
keeps the working directory, logs, message log, and `site/` directory next to
the binary. System packages install the UI at `/usr/share/hodeco/site`, which
matches the Linux default.

If you only want the API, you can point `httpServer.static.dir` somewhere else
or leave it at an empty directory; the gateway still runs, and the UI reports a
missing index.

## First-run check

After the process starts:

1. `GET http://127.0.0.1:8180/api/health` should return healthy.
2. The UI should load at `http://127.0.0.1:8180/`.
3. The server log should report the listening address.
4. Add your Z-Wave serial service, either in the UI or in `services.yaml`
   (next to `config.yaml`):

   ```yaml
   services:
     - alias: ZW
       protocol: zwave
       transport: serial
       entry: /dev/ttyUSB0   # COM4 on Windows
       params:
         baudRate: 115200
   ```

5. Open the messages view. You should start seeing frames when the controller
   is connected.

## From source

Prerequisites:

- Rust 1.95 or newer with the 2024 edition.
- Windows: Visual Studio C++ build tools and the Windows SDK (`mc.exe` and
  `rc.exe` are used for the Event Log message table).
- Linux: a C toolchain is not required for the default build.

```sh
cargo build --workspace
cargo test --workspace
cargo run -- run
```

The backend is cross-checked for Linux with:

```sh
cargo check --target x86_64-unknown-linux-gnu --all-targets
```

The shared contract is checked as a WASM-compatible dependency with:

```sh
cargo check -p hodeco-contract --target wasm32-unknown-unknown
```

## Build the web UI

The UI is a separate workspace under `hodeco-ui/`. The build needs the
`wasm32-unknown-unknown` target and a `wasm-bindgen` CLI version that matches
`hodeco-ui/Cargo.lock`:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130
```

Then run the platform script from `hodeco-ui`:

```powershell
.\scripts\build-web.ps1
```

```sh
./scripts/build-web.sh
```

The output is written to `hodeco-ui/dist`. It contains raw assets, Brotli
sidecars, and gzip sidecars. Point `httpServer.static.dir` at that directory.
Precompressed serving is enabled by default; clients without Brotli or gzip
still receive the raw files.

The browser connects to `ws(s)://<page-host>/api/ws` by default; use `?server=`
only for remote gateways. See the
[UI README](../blob/HEAD/hodeco-ui/README.md) for details and limitations.

## Install as a service

### Linux packages

```sh
sudo dpkg -i hodeco_<version>_amd64.deb
# or
sudo rpm -i hodeco-<version>-1.x86_64.rpm
```

Then register the systemd unit:

```sh
sudo hodeco install [--config /path/to/config.yaml] [--user hodeco]
sudo hodeco start
sudo hodeco status
```

Packages do not install or start the service automatically.

### Windows

From an elevated prompt:

```text
hodeco install
hodeco start
hodeco status
hodeco stop
hodeco uninstall
```

The Windows service runs as `LocalSystem`. An explicit `--config` path is
converted to an absolute path during installation.

### Service details

`--user` defaults to `SUDO_USER`, then `USER`. The account must exist. A warning
is printed when it is not a member of `dialout`, `uucp`, or `plugdev`.

The unit uses `LogsDirectory=hodeco` and `StateDirectory=hodeco`, creating
`/var/log/hodeco` and `/var/lib/hodeco` for the service user. The installer
warns when `/etc/hodeco/config.yaml` is not writable by a non-root service user.
Use `--config` with a writable path if the default config must remain read-only.

## If something does not work

- **Port already in use.** Change `httpServer.port` in the config.
- **The UI is blank or 404.** Check that `httpServer.static.dir` points at the
  extracted `site/` directory or at `hodeco-ui/dist`, and that `index.html`
  exists there.
- **Permission denied on the serial port.** Add the service user to `dialout`,
  `uucp`, or `plugdev`, then sign out and back in.
- **Config mutations do not persist.** The service user needs write access to
  the config file. Use a writable `--config` path or adjust permissions.
- **Windows service commands fail.** Run the prompt as Administrator.
- **A release has no UI.** The portable Linux archive expects
  `--config config.yaml`; without it, Linux defaults point at `/var/lib/hodeco`
  and `/usr/share/hodeco/site`.

## Next steps

- [Configuration](Configuration)
- [HTTP API](HTTP-API)
- [WebSocket](WebSocket)
- [Logging](Logging)
