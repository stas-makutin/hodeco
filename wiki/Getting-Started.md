# Getting Started

This page takes you from a downloaded archive (or a source checkout) to a
running gateway that is talking to your controller. It is written as a path, not
a reference: follow the steps in order, and use the checkpoints to confirm each
one before moving on.

By the end you will have:

- hodeco running on `http://127.0.0.1:8180/`;
- the web UI loaded in a browser;
- at least one Z-Wave or ZigBee service connected;
- frames appearing in the Messages view.

You do not need to install Rust to use a release, and you do not need to write a
configuration file before the first start. The Windows archive uses the platform
defaults; the Linux archive and the packages include a ready-to-run config.

## Before you start

You need:

- a Z-Wave or ZigBee (Ember EZSP) serial controller;
- a machine that stays on — a Raspberry Pi, a mini-PC, or a Windows box;
- permission to open the serial port and write the configuration and data
  directories;
- a browser on the same machine or network.

### Find your serial port

Each service entry needs a serial port name. You can find it with your operating
system, or let the **Discovery** view probe for it when you already know the
protocol and baud rate.

**Windows.** Open Device Manager and look under **Ports (COM & LPT)**. Or run:

```powershell
Get-CimInstance Win32_SerialPort | Select-Object DeviceID, Description
```

**Linux.** Look for a stable by-id path first:

```sh
ls -l /dev/serial/by-id/
```

If there is no by-id entry, list the common device names:

```sh
ls /dev/ttyUSB* /dev/ttyACM*
```

`/dev/ttyUSB0` and `/dev/ttyACM0` are typical, but the number can change after a
replug. Prefer `/dev/serial/by-id/...` in a permanent configuration.

If you already know the protocol and baud rate, the **Discovery** view in the
Services tab can probe the host's serial ports and report which ones respond.
It is a port scan, not device pairing. A ZigBee probe includes a coordinator
reset and EZSP version negotiation, so each ZigBee port can take a few seconds.

**Permissions.** The account running hodeco must be able to open the port. On
Linux that usually means membership in `dialout`, `uucp`, or `plugdev`. The
packages add the `hodeco` service account automatically; for a manual install,
add your user and sign out and back in.

### Choose a path

| Path | Best for |
| --- | --- |
| Release archive | A quick look, a portable install, or Windows. |
| `.deb` / `.rpm` | A system-wide Linux install with a systemd service. |
| Source | Development, a custom build, or a platform we do not package. |

All three end at the same place: a gateway on `http://127.0.0.1:8180/` serving
the web UI from the same process.

## Path 1: run a release

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

`run` starts the gateway in the foreground. Leave it running and open
<http://127.0.0.1:8180/>. The default bind address is `0.0.0.0`, so from another
machine on the network use the host's address instead, for example
<http://192.168.1.5:8180/>.

On Windows the default static directory is `<exe_dir>\site`, so the bundled UI
is used automatically. Linux portable archives include a `config.yaml` that
keeps the working directory, logs, message log, and `site/` directory next to
the binary. System packages install the UI at `/usr/share/hodeco/site`, which
matches the Linux default.

If you only want the API, you can point `httpServer.static.dir` somewhere else
or leave it at an empty directory. The gateway still runs; the UI will report a
missing index.

## First-run check

Work through these checkpoints after the process starts.

1. **Health.** Open <http://127.0.0.1:8180/api/health>. It should return
   `{"status":"ok"}`.
2. **UI.** Open <http://127.0.0.1:8180/>. You should see the hodeco shell and
   the top-level tabs.
3. **Log.** The operational log should report the listening address. The file
   location is in the [Configuration](Configuration) defaults table.
4. **Service.** Add your controller in the **Services** view, or edit
   `services.yaml` next to `config.yaml`:

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

   Saves made in the UI apply immediately. If you edit the file yourself,
   restart hodeco; there is no file watcher.

   The baud rates above are common defaults, not universal. Check the
   controller's manual. The HUSBZB-1 ZigBee side works with
   `flowControl: none` at 57600 8N1; use `software` if your stick or firmware
   requires XON/XOFF.

5. **Messages.** Open the **Messages** view. With the controller connected you
   should start seeing frames. ZigBee coordinator messages are decoded as EZSP;
   ASH ACKs and NAKs appear as raw link messages, and responses/callbacks
   include nested ZCL fields where applicable.

If the service is listed but stays disconnected, check the serial port name,
permissions, and the operational log. The [troubleshooting
section](#if-something-does-not-work) at the bottom of this page covers the
common cases.

## Path 2: install a Linux package

The `.deb` and `.rpm` packages install a system-wide service and prepare the
directories for you:

```sh
sudo apt install ./hodeco_<version>_amd64.deb
# or
sudo dnf install ./hodeco-<version>-1.x86_64.rpm
```

The package creates or repairs the `hodeco` system user and group, adds the
account to `dialout`, `uucp`, and `plugdev` when those groups exist, and prepares
`/etc/hodeco`, `/var/lib/hodeco`, and `/var/log/hodeco` with the correct
ownership.

It enables `hodeco.service` and starts it on a fresh install. On an upgrade it
stops the running service before replacing files and starts it again only if it
had been running.

```sh
sudo systemctl status hodeco
sudo systemctl restart hodeco
```

`/etc/hodeco/config.yaml` is a package configuration file, so local edits
survive upgrades. Uninstalling stops and disables the unit but keeps the
`hodeco` user and the data directories for a later reinstall.

For the package layout and which file to download, see [Releases](Releases).

## Path 3: build from source

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

### Build the web UI

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

The browser connects to `ws(s)://<page-host>/api/ws` by default; use
`?gateway_url=` for a remote gateway or a reverse-proxy prefix. See the
[UI README](../blob/HEAD/hodeco-ui/README.md) for details and limitations.

## Install as a service

### Linux packages

This is the path described in [Path 2](#path-2-install-a-linux-package). Use it
unless you need a portable archive.

### Portable archive

From an extracted release archive:

```sh
sudo hodeco install [--config /path/to/config.yaml] [--user hodeco]
sudo hodeco start
sudo hodeco status
```

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

For a manually installed unit, `--user` defaults to `SUDO_USER`, then `USER`.
The account must exist. A warning is printed when it is not a member of
`dialout`, `uucp`, or `plugdev`. If the unit was installed by a package,
`hodeco install` and `hodeco uninstall` leave it alone; manage the package
instead.

The unit uses `LogsDirectory=hodeco` and `StateDirectory=hodeco`, creating
`/var/log/hodeco` and `/var/lib/hodeco` for the service user. The manual
installer warns when `/etc/hodeco/config.yaml` is not writable by a non-root
service user. Use `--config` with a writable path if the default config must
remain read-only. The packages prepare `/etc/hodeco` for write-back instead.

## If something does not work

Start with the operational log. It usually says whether the port opened, which
address the server bound, and why a configuration file was rejected.

| Symptom | Likely cause | What to try |
| --- | --- | --- |
| `Port already in use` | Another process is using the HTTP port. | Change `httpServer.port` in the config. |
| The UI is blank or returns 404 | The static directory is wrong or `index.html` is missing. | Point `httpServer.static.dir` at the extracted `site/` directory or `hodeco-ui/dist`, and check that `index.html` exists. |
| `Permission denied` on the serial port | The service account is not in the serial group. | Add the user to `dialout`, `uucp`, or `plugdev`, then sign out and back in. For a package install, the `hodeco` user is handled for you. |
| The service stays disconnected | Wrong port, wrong baud rate, wrong flow control, or the controller is unplugged. | Check the entry and params, then check the log for the open error. Try the by-id path on Linux. |
| Config changes do not persist | The service user cannot write the config or catalog file. | Use a writable `--config` path or adjust permissions. Package installs make `/etc/hodeco` group-writable. |
| Windows service commands fail | The prompt is not elevated. | Run the prompt as Administrator. |
| A Linux release has no UI | The portable archive was started without its config, so Linux defaults point at `/var/lib/hodeco` and `/usr/share/hodeco/site`. | Run from the extracted folder with `--config config.yaml`. |
| Frames do not appear | The controller is connected but idle, or the protocol/baud settings are wrong. | Open the Services view and check the state; then check the log for decode or timeout errors. |

## Related pages

Now that the gateway is running, pick the page that matches what you want to do:

- [Configuration](Configuration) — the five files, the full reference, and the
  service parameters.
- [HTTP API](HTTP-API) — make your first request and find the method you need.
- [WebSocket](WebSocket) — subscribe to live events.
- [Logging](Logging) — learn which log answers which question.
- [Tasks](Tasks) and [Schedules](Schedules) — add automation.
