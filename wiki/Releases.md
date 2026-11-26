# Releases

Published builds are available on the [Releases page](../releases). The
maintainer process is documented in [`RELEASING.md`](../blob/HEAD/RELEASING.md).

This page helps you pick the right file, understand what is inside it, and
upgrade without losing your configuration.

## Which file should I download?

| You have | Pick |
| --- | --- |
| Windows 10/11 x64 | `hodeco-<version>-windows-x86_64.zip` |
| Windows on ARM | `hodeco-<version>-windows-aarch64.zip` |
| A normal Linux x64 machine | `linux-x86_64.tar.gz`, or the `amd64.deb` |
| A 64-bit Raspberry Pi or ARM server | `linux-aarch64.tar.gz`, or the `arm64.deb` |
| A Raspberry Pi 1 / Zero / 32-bit Raspberry Pi OS (ARMv6) | `linux-armv6.tar.gz` |
| A 32-bit ARMv7 Linux (for example Debian or Ubuntu armhf) | `linux-armv7.tar.gz`, or the `armhf.deb` |
| A minimal container or older distro | `linux-x86_64-musl.tar.gz` or `linux-aarch64-musl.tar.gz` |

The `.tar.gz` files are portable archives. The `.deb`/`.rpm` files are
system-wide packages with a conventional install layout.

## Supported targets

| Platform | Target | Portable archive | Package |
| --- | --- | --- | --- |
| Windows x64 (static CRT) | `x86_64-pc-windows-msvc` | `.zip` | — |
| Windows ARM64 (static CRT) | `aarch64-pc-windows-msvc` | `.zip` | — |
| Linux x64 (glibc) | `x86_64-unknown-linux-gnu` | `.tar.gz` | `.deb`, `.rpm` |
| Linux ARM64 (glibc) | `aarch64-unknown-linux-gnu` | `.tar.gz` | `.deb`, `.rpm` |
| Linux x64 (static) | `x86_64-unknown-linux-musl` | `.tar.gz` | — |
| Linux ARM64 (static) | `aarch64-unknown-linux-musl` | `.tar.gz` | — |
| Linux ARMv7 (glibc) | `armv7-unknown-linux-gnueabihf` | `.tar.gz` | `.deb`, `.rpm` |
| Linux ARMv6 (static) | `arm-unknown-linux-musleabihf` | `.tar.gz` | — |

Windows builds link the MSVC C runtime statically and do not need the Visual
C++ Redistributable. Musl builds are fully static and do not depend on the host
glibc. Both are useful on machines without the usual runtime packages.

The ARMv6 build is the right choice for Raspberry Pi 1, Zero, Zero W, and
32-bit Raspberry Pi OS, whose userland is ARMv6. The ARMv7 build requires an
ARMv7 userland (for example Debian or Ubuntu armhf) and will not run on those
ARMv6 systems.

## After you download

For a portable archive, extract it and run the binary in the foreground:

```powershell
# Windows
Expand-Archive hodeco-<version>-windows-x86_64.zip -DestinationPath hodeco
cd hodeco
.\hodeco.exe run
```

```sh
# Linux
tar -xzf hodeco-<version>-linux-x86_64.tar.gz
cd hodeco-<version>-linux-x86_64
./hodeco run --config config.yaml
```

Then open <http://127.0.0.1:8180/>. [Getting Started](Getting-Started) has the
first-run checklist and the serial-port setup.

Windows builds use the static MSVC CRT, so no Visual C++ Redistributable is
required. To install the Windows service, run from an elevated prompt:

```text
hodeco install
hodeco start
```

The service runs as `LocalSystem`. An explicit `--config` path is converted to
an absolute path during installation.

## Archive contents

Portable archives are named `hodeco-<version>-<label>` and contain:

```text
hodeco(.exe)
site/                     # raw web assets plus .br and .gz sidecars
config.yaml               # Linux archives only; portable relative paths
README.md
CHANGELOG.md
LICENSE-MIT
LICENSE-APACHE
THIRD-PARTY-LICENSES.md
THIRD-PARTY-LICENSES-UI.md
```

The web assets are shipped raw plus Brotli and gzip sidecars. The raw files are
the fallback for clients that accept neither encoding. Licenses are
application-level files, not part of `site/`.

The backend's default static directory is `<exe_dir>\site` on Windows and
`/usr/share/hodeco/site` on Linux. The Windows archive therefore works with the
defaults. Linux archives include a `config.yaml` that sets the working
directory, logs, message log, and static directory relative to the extracted
folder. Run from that folder with `--config config.yaml`.

## Packages

For glibc Linux targets, the release also includes `.deb` and `.rpm` packages.
They install:

```text
/etc/hodeco/config.yaml
/usr/bin/hodeco
/usr/lib/systemd/system/hodeco.service
/usr/lib/sysusers.d/hodeco.conf
/usr/share/hodeco/site/
/usr/share/doc/hodeco/
```

The documentation directory contains the app licenses and both backend and
frontend third-party notices (`THIRD-PARTY-LICENSES.md` and
`THIRD-PARTY-LICENSES-UI.md`). The configuration file is installed as a package
configuration file, so local edits survive upgrades.

Install with the normal package manager:

```sh
sudo apt install ./hodeco_<version>_amd64.deb
# or
sudo dnf install ./hodeco-<version>-1.x86_64.rpm
```

RPM packages use the Fedora/RHEL dependency names (`glibc`, `libgcc`, and
`systemd`). On distributions with different package names, use the static musl
archive instead of the RPM.

The package creates or repairs the `hodeco` system user and group, adds the
account to `dialout`, `uucp`, and `plugdev` when those groups exist, and
prepares `/etc/hodeco`, `/var/lib/hodeco`, and `/var/log/hodeco` with the
correct ownership. It enables `hodeco.service` and starts it on a fresh
install. On an upgrade it stops the running service before replacing files and
starts it again only if it had been running. While the package owns the unit,
`hodeco install` and `hodeco uninstall` leave it alone; use the package manager
instead.

```sh
sudo systemctl status hodeco
sudo systemctl restart hodeco
```

`/etc/hodeco` is group-writable by `hodeco` so configuration and catalog
write-back through the API works. Uninstalling stops and disables the unit but
keeps the `hodeco` user and the data directories for a later reinstall.

For a portable archive, register the systemd unit manually with
`hodeco install`; see [Getting Started](Getting-Started). The default config
path is `/etc/hodeco/config.yaml`.

## Upgrading

**With a package:** use the package manager as usual. The package stops the
running service before replacing files and starts it again only if it had been
running. `/etc/hodeco/config.yaml` is preserved, so local edits survive.

**With a portable archive:** stop the old process or service, extract the new
archive to a new directory, and copy over the files you want to keep:

- `config.yaml` and the four catalogs;
- `messages.log`;
- the `tasks/` history directory, if you use task history.

Then start the new binary with the same `--config` path. If the config uses
relative paths, keep the same directory layout, or switch to absolute paths
before moving.

**From 0.1.x:** archive or delete the old message log before upgrading; see
[Persistence](Persistence). Protocol compatibility is tracked separately through
`server.info.protocolVersion`, currently `1.0`.

## Portable archive notes

The generated `log/` directory and `messages.log` live next to the binary when
using the Linux portable config. The portable `config.yaml` uses relative paths
and is meant for foreground runs from the extracted directory.

Use `linux-armv6` for Raspberry Pi 1/Zero and 32-bit Raspberry Pi OS, and
`linux-armv7` for 32-bit ARMv7 systems with a glibc userland.

For a systemd service, prefer the `.deb`/`.rpm` package. The ARMv6 build is
portable-only; for it, use the installer below. If you must run the service
from a portable directory, create a config with absolute paths and pass it to
the installer:

```sh
sudo ./hodeco install --config /path/to/config.yaml
sudo hodeco start
```

The installer uses the current executable path in the unit, so install from the
location where the binary should remain. The service account must be able to
traverse and write the configured working, log, and message-log paths.

## Verifying downloads

Each release includes `SHA256SUMS`:

```sh
sha256sum --check SHA256SUMS
```

On Windows, compare the output of:

```powershell
Get-FileHash .\hodeco-<version>-windows-x86_64.zip -Algorithm SHA256
```

## Versioning

Release tags are `v<version>` and must match both workspace versions
(`Cargo.toml` and `hodeco-ui/Cargo.toml`). Pre-release tags such as
`v0.3.0-rc.1` are marked as pre-releases on GitHub. The GitHub release notes are
the `CHANGELOG.md` section for the tag version. Protocol compatibility is
tracked separately through `server.info.protocolVersion`, currently `1.0`.

## Related pages

- [Getting Started](Getting-Started) — first run and service installation.
- [Configuration](Configuration) — the files to keep across upgrades.
- [Persistence](Persistence) — what survives a restart and what to back up.
- [Development](Development) — building from source.
