# Releases

Published builds are available on the [Releases page](../releases). The
maintainer process is documented in [`RELEASING.md`](../blob/HEAD/RELEASING.md).

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
| Windows x64 | `x86_64-pc-windows-msvc` | `.zip` | — |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `.zip` | — |
| Linux x64 (glibc) | `x86_64-unknown-linux-gnu` | `.tar.gz` | `.deb`, `.rpm` |
| Linux ARM64 (glibc) | `aarch64-unknown-linux-gnu` | `.tar.gz` | `.deb`, `.rpm` |
| Linux x64 (static) | `x86_64-unknown-linux-musl` | `.tar.gz` | — |
| Linux ARM64 (static) | `aarch64-unknown-linux-musl` | `.tar.gz` | — |
| Linux ARMv7 (glibc) | `armv7-unknown-linux-gnueabihf` | `.tar.gz` | `.deb`, `.rpm` |
| Linux ARMv6 (static) | `arm-unknown-linux-musleabihf` | `.tar.gz` | — |

Musl builds are fully static and do not depend on the host glibc. They are
useful for minimal containers and older distributions.

The ARMv6 build is the right choice for Raspberry Pi 1, Zero, Zero W, and
32-bit Raspberry Pi OS, whose userland is ARMv6. The ARMv7 build requires an
ARMv7 userland (for example Debian or Ubuntu armhf) and will not run on those
ARMv6 systems.

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
/usr/bin/hodeco
/usr/share/hodeco/site/
/usr/share/doc/hodeco/
```

The documentation directory contains the app licenses and both backend and
frontend third-party notices (`THIRD-PARTY-LICENSES.md` and
`THIRD-PARTY-LICENSES-UI.md`).

Install with the normal package manager:

```sh
sudo dpkg -i hodeco_<version>_amd64.deb
# or
sudo rpm -i hodeco-<version>-1.x86_64.rpm
```

RPM packages use the Fedora/RHEL dependency names (`glibc`, `libgcc`). On
distributions with different package names, use the static musl archive instead
of the RPM.

Packages do not install or start the service automatically. Register the
systemd unit after installing:

```sh
sudo hodeco install
sudo hodeco start
sudo hodeco status
```

`hodeco install` accepts `--config` and `--user`; see
[Getting Started](Getting-Started). The default config path is
`/etc/hodeco/config.yaml`.

## Windows

Extract the archive and run the executable:

```powershell
Expand-Archive hodeco-<version>-windows-x86_64.zip -DestinationPath hodeco
cd hodeco
.\hodeco.exe run
```

Windows builds use the static MSVC CRT, so no Visual C++ Redistributable is
required. To install the Windows service, run from an elevated prompt:

```text
hodeco install
hodeco start
```

The service runs as `LocalSystem`. An explicit `--config` path is converted to
an absolute path during installation.

## Linux portable

```sh
tar -xzf hodeco-<version>-linux-x86_64.tar.gz
cd hodeco-<version>-linux-x86_64
./hodeco run --config config.yaml
```

The generated `log/` directory and `messages.log` live next to the binary when
using this config. The portable `config.yaml` uses relative paths and is meant
for foreground runs from the extracted directory.

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
`v0.2.0-rc.1` are marked as pre-releases on GitHub. Protocol compatibility is
tracked separately through `server.info.protocolVersion`, currently `1.0`.
