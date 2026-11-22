# Releasing hodeco

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml)
and published to GitHub Releases. The wiki is synced from [`wiki/`](wiki/) by
[`.github/workflows/wiki.yml`](.github/workflows/wiki.yml).

## Versioning

The project uses one version for the root workspace and one for the separate
`hodeco-ui` workspace. The release tag must match both:

- `Cargo.toml` → `[workspace.package] version`
- `hodeco-ui/Cargo.toml` → `[workspace.package] version`

All crates in the root workspace inherit the root version (`hodeco`,
`hodeco-contract`, `hodeco-codec`, and `hodeco-schedule`), and `hodeco-ui`
inherits its workspace version. Tags use a `v` prefix: `v0.2.0`, or
`v0.2.0-rc.1` for a pre-release.

The release workflow refuses to build or publish when the tag, root workspace,
and UI versions do not agree.

## One-time GitHub setup

1. Push the repository to GitHub.
2. In **Settings → Features**, enable **Wikis**.
3. Create the first wiki page in the GitHub UI. GitHub creates the wiki git
   repository only after the first page exists; the sync workflow cannot clone
   it before that.
4. Create a **classic personal access token** with `repo` scope (or
   `public_repo` for a public repository). A fine-grained token also works when
   it has **Contents: Read and write** on both the main repository and the
   `.wiki.git` repository, and either token format is accepted by the sync
   workflow. Authorize the token for the organization's SSO if required.
5. Add the token as an Actions secret named `WIKI_TOKEN`
   (**Settings → Secrets and variables → Actions**).

No other secrets are required. Releases use the built-in `GITHUB_TOKEN` with
`contents: write` permission.

The wiki workflow treats `wiki/` as the source of truth: it replaces the wiki
contents on every run, so edit pages in `wiki/` rather than in the GitHub wiki
UI. It runs on pushes to `master` or `main`; update the branch filter in
`.github/workflows/wiki.yml` if the default branch changes.

## Release checklist

1. Update both workspace versions listed above and add the release notes to
   `CHANGELOG.md`.
2. Refresh the lock files and run the local checks:

   ```sh
   cargo check --workspace
   cargo check --manifest-path hodeco-ui/Cargo.toml
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

3. Commit the version bump and lock-file changes.
4. Tag and push:

   ```sh
   git tag -a v0.2.0 -m "hodeco 0.2.0"
   git push origin master --follow-tags
   ```

   Replace `master` with the default branch name. The tag push starts the
   release workflow.

5. Watch the **Release** workflow. It validates the versions, runs formatting,
   clippy, tests, and the wasm contract check, builds the web UI once, and then
   builds every target in parallel. The final job creates the GitHub release
   and attaches all archives, packages, and `SHA256SUMS`.

To build without publishing, run the **Release** workflow manually from the
Actions tab (`workflow_dispatch`). A manual run on a branch keeps the artifacts
as workflow artifacts and skips the release job.

## Package maintainer

The `.deb`/`.rpm` maintainer comes from the top-level `HODECO_MAINTAINER`
environment variable in `.github/workflows/release.yml`. It currently uses
GitHub's no-reply address, which is valid package metadata but does not receive
email; replace it with a real contact address if users should be able to reach
you.

## Targets and artifacts

| Target | Runner | Portable archive | Linux packages |
| --- | --- | --- | --- |
| `x86_64-pc-windows-msvc` | `windows-latest` | `.zip` | — |
| `aarch64-pc-windows-msvc` | `windows-latest` | `.zip` | — |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `.tar.gz` | `.deb`, `.rpm` |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` | `.tar.gz` | `.deb`, `.rpm` |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | `.tar.gz` | — |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | `.tar.gz` | — |
| `armv7-unknown-linux-gnueabihf` | `ubuntu-latest` | `.tar.gz` | `.deb`, `.rpm` |

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

Release web assets contain raw files plus `.br` and `.gz` sidecars. The raw
files remain as a fallback for clients that accept neither encoding. Licenses
are kept at the application level, not inside `site/`.

Packages install the same files under:

```text
/usr/bin/hodeco
/usr/share/hodeco/site/
/usr/share/doc/hodeco/
```

Package documentation includes both `THIRD-PARTY-LICENSES.md` (backend) and
`THIRD-PARTY-LICENSES-UI.md` (frontend).

Package filenames follow the normal conventions, for example:

```text
hodeco_0.2.0_amd64.deb
hodeco-0.2.0-1.x86_64.rpm
hodeco_0.2.0_armhf.deb
```

Windows builds use the static MSVC CRT, so the ZIPs do not require the Visual
C++ Redistributable. Musl builds are fully static and only get portable
archives. The `armv7` package architecture is `armhf` for Debian and `armv7hl`
for RPM. RPM
dependencies use Fedora/RHEL names (`glibc`, `libgcc`); musl archives are the
distro-independent fallback.

## Installing release artifacts

Windows (portable, static CRT):

```powershell
Expand-Archive hodeco-0.2.0-windows-x86_64.zip -DestinationPath hodeco
cd hodeco
.\hodeco.exe run
```

Linux (portable):

```sh
tar -xzf hodeco-0.2.0-linux-x86_64.tar.gz
cd hodeco-0.2.0-linux-x86_64
./hodeco run --config config.yaml
```

Linux package:

```sh
sudo dpkg -i hodeco_0.2.0_amd64.deb
# or
sudo rpm -i hodeco-0.2.0-1.x86_64.rpm
```

Packages do not install or start the service automatically. Register the
systemd unit after installing:

```sh
sudo hodeco install
sudo hodeco start
```

See [the wiki](../../wiki/Releases) for operator-facing notes.

## Release contents and checksums

Each release includes `SHA256SUMS`. Verify downloads before installing:

```sh
sha256sum --check SHA256SUMS
```

On Windows:

```powershell
Get-FileHash .\hodeco-0.2.0-windows-x86_64.zip -Algorithm SHA256
```

## Troubleshooting

- **Tag mismatch:** the tag must be exactly `v` + the workspace version.
  Delete the tag (`git tag -d`, `git push --delete`) and retag.
- **Wiki clone fails:** create the first wiki page in the GitHub UI, confirm
  `WIKI_TOKEN` is set, and re-run the Wiki workflow.
- **A release already exists:** delete the draft/release with
  `gh release delete <tag>`, or upload replacements with
  `gh release upload <tag> <files> --clobber`.
- **Cross-build failures:** check the matrix entry in
  `.github/workflows/release.yml`. GNU ARM targets use the apt cross linkers;
  musl targets use Zig through `cargo-zigbuild`.
- **Windows ARM64 build fails with error 216** ("This version of %1 is not
  compatible with the version of Windows you're running"): the MSVC environment
  is using ARM64-hosted tools on the x64 runner. Configure it with
  `arch: amd64_arm64` in `ilammy/msvc-dev-cmd`, not `arch: arm64`.
- **Windows build invokes `/usr/bin/link`** ("missing operand after ..."): the
  build ran under Git Bash, whose `link.exe` shadows the MSVC linker. Use
  `shell: pwsh` for Windows build steps.
