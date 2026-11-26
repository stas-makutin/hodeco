# How hodeco works

If you are new to hodeco, read this page before the reference pages. It explains
the words the rest of the documentation uses and shows how the pieces fit
together. You do not need any of it to start the gateway, but it makes the
configuration and API pages much easier to follow.

## The short version

hodeco is a gateway between one or more serial controllers and the rest of your
network. A controller is the USB stick or serial adapter that talks to your home
devices using a protocol such as Z-Wave or ZigBee. hodeco owns the serial port,
keeps the device state and message history, and exposes both through one API and
one web UI. Other programs never open the port themselves; they ask hodeco.

That is the whole idea. Everything else is a detail of how the gateway is
configured or how you talk to it.

## The moving parts

| Term | What it means |
| --- | --- |
| **Gateway** | The running `hodeco` process. It owns the configuration, the serial connections, the HTTP/WebSocket server, the message store, logging, and automation. |
| **Service** | One controller connection. A service has an `alias` (a short name such as `ZW`), a `protocol` (`zwave` or `zigbee`), a `transport` (`serial` today), an `entry` (the serial port), and protocol parameters. |
| **Controller** | The physical stick or adapter behind a service. |
| **Device or node** | A home device that the controller can reach. Z-Wave calls them nodes; ZigBee also uses nodes, with endpoints on top. |
| **Message** | One frame or event in either direction, with metadata and a decoded view. Message history is what the Messages view and `messages.*` API operate on. |
| **Discovery** | A serial-port scan. hodeco probes the available ports with the selected protocol and reports which ones respond, so you can pick an entry for a service. It does not pair devices. |
| **Command** | An executable on the gateway machine, declared in `commands.yaml`. Tasks can run it and capture its output. This is different from a protocol command. |
| **Schedule** | A reusable "when" definition in `schedules.yaml` — a time, a solar event, a recurrence, or a one-shot. |
| **Task** | A reusable automation recipe in `tasks.yaml`: triggers start it, actions run in order, and next rules can start other tasks. |
| **Trigger** | What starts a task run: a schedule, an incoming message, an HTTP hook, or a manual/API call. |
| **Action** | One step inside a run: run a command or send a message. |
| **Run** | One execution of a task, with an outcome, action results, and optional history. |
| **Catalog** | In this documentation, usually one of the YAML files (`services.yaml`, `tasks.yaml`, `schedules.yaml`, `commands.yaml`). The codec JSON files are called **codec data files** instead. |

## A message's journey

This is the path a frame takes through the gateway:

1. The controller sends bytes over the serial port.
2. The service reads the frame and adds protocol and service metadata. The UI
   uses `hodeco-codec` to decode the bytes for display.
3. The gateway stores the message in a bounded in-memory history and persists it
   to the message log on a timer.
4. Clients subscribed to the `messages` topic receive `events.messageNew` (or a
   state-change notification).
5. The UI updates its list, and `messages.list` can page or filter the history
   later.
6. If any task has a matching message trigger, the automation engine starts a
   run.

Outgoing messages take the same path in reverse: a client calls
`services.send`, the service frames the payload for the protocol, and the
controller writes it to the serial port.

## Where things live

The five YAML files are siblings by default. The core file points at the other
four, so you can move them if you keep the paths in sync.

| File | What you put there |
| --- | --- |
| `config.yaml` | Core settings: server bind and port, logging, message history, file locations, task-engine limits. |
| `services.yaml` | One entry per controller. This is where the serial port and protocol parameters live. |
| `commands.yaml` | External executables that tasks can run. |
| `schedules.yaml` | Shared location/timezone defaults and reusable recurrence definitions. |
| `tasks.yaml` | Triggers, actions, limits, and next rules for your automation. |

At runtime hodeco also writes:

| Path | What it is |
| --- | --- |
| `hodeco.log` | The operational log: startup, configuration, HTTP, serial, and errors. |
| `messages.log` | The persisted message history. |
| `tasks/` | Per-task run history, only for tasks that enable it. |
| `site/` | The compiled web UI served by the gateway. |

See [Configuration](Configuration) for the platform defaults and full reference,
and [Persistence](Persistence) for the difference between the two histories.

## What do you want to do?

| Goal | Go to |
| --- | --- |
| Run hodeco for the first time | [Getting Started](Getting-Started) |
| Connect a Z-Wave or ZigBee controller | [Getting Started](Getting-Started), then [Configuration](Configuration) |
| Browse frames and decoded payloads | The **Messages** view in the UI, or [HTTP API](HTTP-API) |
| Automate something on a schedule | [Schedules](Schedules), then [Tasks](Tasks) |
| React to an incoming message | [Tasks](Tasks) |
| Call hodeco from a script | [HTTP API](HTTP-API) |
| Build a live client | [WebSocket](WebSocket) |
| Label devices and tweak the protocol builder | [Codec Data Files](Data-Files) |
| Back up or reset history | [Persistence](Persistence) |
| Install as a service | [Getting Started](Getting-Started) or [Releases](Releases) |
| Work on the code | [Development](Development) |

## A few terms that trip people up

- **"Service" is overloaded.** In hodeco, a service is one controller entry. The
  Windows service or systemd unit is called "the hodeco service" in this
  documentation when the distinction matters.
- **Discovery is not device pairing.** It probes serial ports and reports which
  ones respond to the selected protocol. Pairing devices is a separate protocol
  operation.
- **A command is an executable.** A Z-Wave or ZigBee command is a protocol
  operation and belongs to the codec data files.
- **A schedule is not a task.** The schedule answers *when*; the task decides
  *what* happens and owns the triggers, actions, and history.
- **History has two meanings.** Message history is the frame log. Task history
  is the opt-in record of automation runs.

## Related pages

- [Getting Started](Getting-Started) — install, run, and connect a controller.
- [Configuration](Configuration) — the five files and their fields.
- [Tasks](Tasks) and [Schedules](Schedules) — automation from the ground up.
- [HTTP API](HTTP-API) and [WebSocket](WebSocket) — build your own client.
