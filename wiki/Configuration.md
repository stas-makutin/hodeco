# Configuration

hodeco is configured by one optional YAML file. With no file at all it starts
with sensible defaults, so the smallest useful configuration is just the serial
service you want to talk to:

```yaml
services:
  - alias: ZW
    protocol: zwave
    transport: serial
    entry: /dev/ttyUSB0   # COM4 on Windows
    params:
      baudRate: 115200
```

Everything else can be left implicit until you need it. Unknown fields are
rejected, so typos fail at startup instead of being silently ignored.

The platform defaults are:

| Item | Windows | Linux |
| --- | --- | --- |
| Config | `<exe_dir>\config.yaml` | `/etc/hodeco/config.yaml` |
| Working directory | `<exe_dir>` | `/var/lib/hodeco` |
| Log file | `<exe_dir>\log\hodeco.log` | `/var/log/hodeco/hodeco.log` |
| Message log | `<exe_dir>\messages.log` | `/var/lib/hodeco/messages.log` |
| Static assets | `<exe_dir>\site` | `/usr/share/hodeco/site` |

Relative paths in the config are resolved after the configured `workingDir` is
created and entered.

## Full reference

This is the complete shape of the file, with the defaults shown:

```yaml
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
  autoPersist: 6 h
  onCorrupt: ignore  # ignore or fail

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

## Notes

- Service aliases and keys must be unique.
- `outgoingMaxTTL` bounds how long an outgoing message may wait in the service
  queue. The default is `10 s`; `0 s` disables the TTL. The message builder can
  override it per send without changing the service configuration.
- `openAttemptsInterval` controls how often a service retries opening its
  transport; the default is `3 s`.
- `services.add`, `services.remove`, and `services.alias` rewrite the config
  file. The service account must be able to write it for write-back to succeed.
- Config changes made outside the service require a manual restart; there is no
  file watcher.
- `log.maxSize: 0` or `log.maxAge: 0 s` disables that rotation trigger.
  Rotation is enabled only when retention is configured.
- `messageLog.onCorrupt` is `ignore` or `fail`.
- `httpServer.static.precompressed` enables serving `.br`/`.gz` sidecars and
  defaults to `true`. Release builds ship raw assets plus `.br` and `.gz`
  sidecars; the raw file remains the fallback for clients that accept neither.
  JSON definition files are always served uncompressed so runtime edits are
  visible. For other assets, regenerate the sidecars when editing raw files; a
  stale sidecar can shadow a newer raw file.

See [Logging](Logging) and [HTTP API](HTTP-API) for the runtime behavior behind
these settings.
