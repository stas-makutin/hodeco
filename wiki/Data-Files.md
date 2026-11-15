# Codec data files

Two JSON files drive the UI's Z-Wave decoder and its device labels:

| File | Purpose |
| --- | --- |
| `hodeco-codec/data/zwave.json` | Serial API commands, command classes, field layouts, enums, and summary templates. |
| `hodeco-codec/data/entities.json` | Human labels and locations for devices, mapped from protocol-native service and node IDs. |

Both files are compiled into `hodeco-codec` as fallbacks and can also be
overridden at runtime.

## Runtime loading

Native builds look for `zwave.json` and `entities.json`:

1. beside the executable;
2. in the current working directory;
3. in the checked-in codec data directory during development.

Web builds fetch both files from the page root, so a release `site/` can ship
edited copies.

Each file is handled independently. A missing file keeps the embedded copy. An
invalid file logs a warning and keeps the embedded copy for that file. Editing an
external file takes effect when the UI is restarted or the page is reloaded.

---

## `zwave.json`

The bundled catalog is version 1 and currently describes 135 command classes
with 377 commands, 141 Serial API commands, and 59 packed groups. Most common
commands have typed layouts; the remaining raw fields are documented fallbacks
for variable-length lists and vendor-specific or union payloads.

### Top level

```json
{
  "version": 1,
  "protocol": "zwave",
  "controlFrames": [],
  "frameTypes": [],
  "serialCommands": [],
  "commandClasses": []
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `version` | integer | Catalog version. The bundled catalog is version 1; bump it only when a published file's schema or content changes meaningfully. |
| `protocol` | string | Must be `zwave`; the UI rejects an external file with a different protocol. |
| `controlFrames`  | array | Single-byte control frames such as ACK, NAK, and CAN. |
| `frameTypes` | array | Data-frame directions (request/response). |
| `serialCommands` | array | Serial API commands and their request/response/callback layouts. |
| `commandClasses` | array | Application command classes and their commands. |

IDs are scoped: serial command IDs must be unique in `serialCommands`, command
class IDs must be unique in `commandClasses`, and command IDs must be unique
within one class.

### Control frames and frame types

```json
{ "id": 6, "name": "ACK", "kind": "ack" }
```

`kind` is `ack`, `nak`, or `can`.

```json
{ "id": 1, "name": "Response", "direction": "response" }
```

`direction` is `request` or `response`.

### Serial commands

```json
{
  "id": 21,
  "name": "ZW_Version",
  "description": "Read the controller's Serial API library version.",
  "requestSummary": "ZW_Version request",
  "fields": [],
  "noRequestParameters": true,
  "responseSummary": "ZW_Version · {libraryVersion}",
  "responseFields": [
    {
      "name": "libraryVersion",
      "label": "Library version",
      "kind": "text",
      "length": 12,
      "description": "Zero-terminated version string.",
      "required": true
    },
    {
      "name": "libraryType",
      "label": "Library type",
      "kind": "enum",
      "required": true,
      "values": [
        { "value": 1, "name": "Static controller" },
        { "value": 2, "name": "Controller" }
      ]
    }
  ]
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | integer | Serial API function ID. |
| `name` | string | Display name. |
| `description` | string, optional | Longer explanation. |
| `priority` | integer, optional | Operation-picker sort priority. Lower values appear first; defaults to `1000`. |
| `builder` | string, optional | Editor used in the Send message dialog: `generic` (default), `sendData`, or `sendDataMulti`. |
| `incomingOnly` | boolean | Incoming notification; decoded but not offered as an outgoing operation. |
| `defaultOperation` | boolean | Selected when the dialog opens for the matching protocol. |
| `emphasize` | boolean | Highlight the operation in the Operation picker. |
| `fields` | array | Request fields; also the builder-facing descriptors. |
| `noRequestParameters` | boolean | The command takes no request parameters. Use this instead of leaving `fields` empty when the layout is simply not documented yet. |
| `requestSummary` | string, optional | Summary template for a request frame. |
| `responseFields` | array, optional | Response layout. |
| `responseSummary` | string, optional | Summary template for a response frame. |
| `callback` | object, optional | Callback layout selected by its exact data length. |

A callback looks like:

```json
{
  "dataLength": 2,
  "summary": "ZW_SendData callback · {txStatus}",
  "fields": []
}
```

`dataLength` is the number of command-parameter bytes that selects this callback
layout. Callback fields and summaries work like request/response fields.

### Operation picker

The **Send message** dialog assembles its Operation list from three sources:

1. `Raw message` — a universal UI base operation. It is always available for
   every protocol, sends bytes exactly as entered without framing, and is first.
2. Z-Wave host-side operations provided by `hodeco-codec`:
   - `Raw Serial API frame` — completes the frame and recalculates length and
     checksum;
   - `ZW_SendData (broadcast)` — SendData to `0xFF` with ACK off and `NO_ROUTE`
     on.
3. Z-Wave `serialCommands` from `zwave.json` that are not marked
   `incomingOnly`.

Serial API names come from `serialCommands` (`ZW_Version`, `ZW_SendData`,
`ApplicationNodeInformation`, and so on), without an extra API prefix.

Entries sort by `priority` (lower first). Equal priorities keep their source
order. The first five entries use the accent colour:

| Operation | Priority | Source |
| --- | --- | --- |
| `Raw message` | 0 | UI base operation |
| `Raw Serial API frame` | 5 | `hodeco-codec` |
| `ZW_SendData` | 10 | `serialCommands` with `builder: "sendData"` |
| `ZW_SendDataMulti` | 20 | `serialCommands` with `builder: "sendDataMulti"` |
| `ZW_SendData (broadcast)` | 30 | `hodeco-codec` |

`ZW_Version` uses the normal default priority, so it stays in its catalog
position.

The `builder` value selects the editor:
- `generic` renders the request `fields` as typed controls;
- `sendData` and `sendDataMulti` show the node, command-class, TX-option, and
  callback editors;
- the codec-provided `broadcast` shows the broadcast editor;
- `Raw Serial API frame` and `Raw message` show their raw inputs.

SendData and SendDataMulti read TX options from the selected serial command's
`txOptions` field. The bundled definitions expose `ACK`, `LOW_POWER`,
`AUTO_ROUTE`, `NO_ROUTE`, and `EXPLORE`; the default is ACK on. Broadcast keeps
the fixed recommended options (ACK off, `NO_ROUTE` on).

The per-send TTL is an **Override TTL** checkbox followed by the same
integer-and-unit editor used by service parameters. Unchecked inherits the
service's `outgoingMaxTTL`, then the protocol default (10 s); checked sends an
explicit value.

Fields referenced by `lengthFrom` or `countFrom` are computed from the data or
node list and shown as read-only values. `derived` fields are emitted
automatically. The command ID, frame length, and checksum are added
automatically.

Commands with `noRequestParameters` show a short note and need no input.
Commands whose request layout is not documented yet keep a raw hex fallback.
The Operation picker opens as a popup below its button. Its filter is focused
when the popup opens, and the result list scrolls independently so the filter
stays visible.

### Command classes

```json
{
  "id": 112,
  "name": "Configuration",
  "description": "Device configuration parameters.",
  "commands": [
    {
      "id": 6,
      "name": "Report",
      "summary": "Configuration · parameter {parameterNumber}",
      "fields": []
    }
  ]
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | integer | Command class ID. |
| `name` | string | Display name. |
| `description` | string, optional | Longer explanation. |
| `commands` | array | Commands in this class. |

A command has `id`, `name`, optional `description`, optional `fields`, and an
optional `summary` template. A command with no fields still shows its class and
command name; the remaining bytes are displayed as raw `CC data`.

The builder's **Command class** picker lists every class in `zwave.json`,
including legacy classes with no commands, and filters by name or code. The
**Command** picker lists that class's commands as a normal dropdown. Commands
with field descriptors use typed controls; other commands accept optional raw
parameter bytes after the command ID. The bundled catalog covers the known
standard command-class ID range, including legacy Security Panel Mode, Security
Panel Zone, and Security Panel Zone Sensor entries. Use **Raw command class
bytes** for a class with no command definitions.

### Fields

Fields are shared by serial commands and command classes.

```json
{
  "name": "libraryVersion",
  "label": "Library version",
  "kind": "text",
  "length": 12,
  "description": "Zero-terminated version string.",
  "required": true
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `name` | string | Stable key used by templates and length/count references. |
| `label` | string | Label shown in the Parsed view. |
| `kind` | string | Decoder type; see below. |
| `description` | string, optional | Field explanation. |
| `required` | boolean | Defaults to `true`. Missing required fields are reported as decode issues. |
| `min`, `max` | integer, optional | Allowed range for numeric fields and builder inputs. |
| `values` | array | `{ "value": number, "name": string, "length": number }` entries for enums and flags. `length` is optional and selects a field width. |
| `mask` | integer, optional | Mask applied before looking up an enum value, or within a packed group. |
| `shift` | integer, optional | Right-shift applied after `mask` in a packed group. Defaults to `0`. |
| `rawSuffix` | boolean | Append the raw byte to enum/flag labels. |
| `unknown` | string, optional | Text used when an enum value is not known. |
| `role` | string, optional | Semantic role: `value` (default), `length`, `target`, `targets`, `source`, or `commandClass`. |
| `format` | string, optional | Display format: `default`, `hex`, `decimal`, `level`, `duration`, `meterRateScale`, or `txOptions`. |
| `length` | integer, optional | Fixed byte length for variable-width fields such as `text` and `hex`. |
| `lengthFrom` | string, optional | Take the byte length from an earlier numeric field. If the source is an enum with `length` metadata, use that mapped length. |
| `countFrom` | string, optional | Take a byte count from an earlier numeric field; used for byte-sized elements such as node lists. |
| `remaining` | boolean | Consume all remaining bytes. |
| `derived` | boolean | The builder computes this field; it is not a direct user input. |

#### Field kinds

| Kind | Meaning |
| --- | --- |
| `node` | One node ID byte. |
| `nodeList` | One or more node ID bytes. |
| `uint8`, `uint16`, `uint32` | Big-endian unsigned integers. |
| `bool` | One byte: `Off` for zero, `On` otherwise. |
| `int` | Signed big-endian integer; width comes from `lengthFrom` or fixed `length`. |
| `uint` | Unsigned big-endian integer; width comes from `lengthFrom` or fixed `length`. |
| `enum` | One byte looked up in `values`, optionally after `mask`. |
| `flags` | Bit set described by `values`. |
| `hex` | Raw bytes rendered as spaced hex. |
| `duration` | Z-Wave duration byte with friendly rendering. |
| `text` | UTF-8 text; trailing NUL padding is trimmed. |
| `txOptions` | Transmit-option bits. |
| `callbackId` | Callback identifier byte. |
| `commandClassData` | Nested command-class data; decoded recursively. |

Numeric values use `uint8`, `uint16`, `uint32`, `int`, or `uint`. `hex` is
reserved for bitmasks, keys, repeated lists, and genuinely variable raw bytes.
For example, Configuration parameter values are signed integers whose width
comes from the `size` enum.

#### Packed groups

A `fields` array can contain a group object that packs several logical fields
into one big-endian integer:

```json
{
  "group": "precisionScaleSize",
  "length": 1,
  "fields": [
    {
      "name": "size",
      "label": "Size",
      "kind": "enum",
      "mask": 7,
      "shift": 0,
      "values": [
        { "value": 0, "name": "1 byte (8 bits)", "length": 1 },
        { "value": 1, "name": "2 bytes (16 bits)", "length": 2 },
        { "value": 2, "name": "4 bytes (32 bits)", "length": 4 }
      ]
    },
    {
      "name": "scale",
      "label": "Scale",
      "kind": "enum",
      "mask": 24,
      "shift": 3,
      "values": [
        { "value": 0, "name": "Celsius" },
        { "value": 1, "name": "Fahrenheit" }
      ]
    },
    {
      "name": "precision",
      "label": "Precision",
      "kind": "uint8",
      "mask": 224,
      "shift": 5,
      "min": 0,
      "max": 7
    }
  ]
}
```

Rules:

- `length` is the group width in bytes.
- Group subfields require `mask` and `shift`.
- Masks must be contiguous, non-overlapping, and fit inside the group width.
- `shift` must match the mask's least-significant set bit.
- Subfields are read and written in order; the group itself is not a field and
  does not appear in decoded output.
- A subfield can be referenced by `lengthFrom`, such as `"lengthFrom": "size"`.
- The group name is diagnostic and is not rendered as a label.
- The same group schema is used by `serialCommands` and `commandClasses`,
  including request, response, and callback layouts.

Bundled packed groups (59 in the current catalog) cover Thermostat Setpoint,
Sensor Multilevel, Meter, Configuration Properties, Multi Channel
endpoint/capability fields, Door Lock handles/conditions/options, Central Scene
key attributes, Supervision session flags, and the Add/Remove/Node ID inclusion
mode flags.

#### Length rules

Only one of the length mechanisms should be used for a field:

- fixed width from `kind` for numeric and enum-like fields;
- `length` for a fixed number of bytes;
- `lengthFrom` for a byte count stored in an earlier numeric field;
- `countFrom` for a byte count stored in an earlier numeric field (for byte-sized elements);
- `remaining: true` to consume the rest of the layout.

`lengthFrom` and `countFrom` must name an earlier field in the same layout whose
kind is numeric. Decode-time issues are reported when a reference is missing.
When the source is an enum with `length` metadata, the selected enum value maps
to the wire length instead of using the raw numeric value. In the **Send
message** dialog, numeric length sources become computed values; enum length
sources stay user-selectable. A fixed-length `nodeList` is padded with zero node
IDs when fewer entries are entered.

#### Summary templates

`requestSummary`, `responseSummary`, `callback.summary`, and command summaries
support these placeholders:

| Placeholder | Value |
| --- | --- |
| `{name}` | The field's display text. |
| `{name:number}` | Numeric value when available, otherwise display text. |
| `{name:hex}` | Hex value when available, otherwise display text. |
| `{commandClassSummary}` | The nested command-class summary. |

Unknown placeholders expand to an empty string. Empty summaries fall back to the
generic `"<command> <direction>"` form.

### Fallback behavior

The decoder is intentionally tolerant:

- unknown serial command IDs show `0xNN` and raw data;
- unknown command classes show `Unknown 0xNN` and raw data;
- known classes with unknown commands show the class name plus the hex command;
- commands without field layouts show raw `CC data`;
- bytes left after a layout are shown as `CC data`;
- malformed frames still produce a frame with decode issues instead of failing.

This is why the catalog can cover a large ID set without every command needing a
fully described layout.

---

## `entities.json`

`entities.json` maps native service/device IDs to readable labels. The top level
is intentionally protocol-neutral; other applications may add their own fields
and preserve them.

### Top level

```json
{
  "version": 1,
  "devices": [],
  "services": {}
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `version` | integer | Catalog version. |
| `devices` | array | Stable device labels. |
| `services` | object | Maps service keys to their device lists. |

### Devices

```json
{
  "id": "office-lamp",
  "label": "Office lamp",
  "location": "Office",
  "vendor": "Example"
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | string | Stable join key. Never shown to the user. |
| `label` | string | Human label. |
| `location` | string, optional | Room or area. |
| other keys | any | Preserved for other applications; ignored by hodeco. |

A device displays as `label (location)` when `location` is non-empty, otherwise
as `label`.

### Service mappings

The `services` object uses one of two key forms:

| Key form | Example | Use |
| --- | --- | --- |
| `alias:<alias>` | `alias:ZW` | Matches a configured service alias. |
| `key:<protocol>/<transport>/<entry>` | `key:zwave/serial/COM3` | Matches the stable service identity and survives alias renames. |

```json
{
  "key:zwave/serial/COM3": {
    "devices": [
      { "device_id": "controller", "service_id": 1 },
      { "device_id": "hallway-dimmer", "service_id": 3 }
    ]
  },
  "alias:ZW": {
    "devices": [
      { "device_id": "controller", "service_id": 1 },
      { "device_id": "office-lamp", "service_id": 2 }
    ]
  }
}
```

A service entry has:

| Key | Type | Meaning |
| --- | --- | --- |
| `devices` | array | Device mappings for this service. |
| other keys | any | Preserved for other applications. |

Each device mapping has:

| Key | Type | Meaning |
| --- | --- | --- |
| `device_id` | string | Must match a device `id` in the top-level `devices` list. |
| `service_id` | number or string | Native ID inside the service. Z-Wave uses numeric node IDs; other protocols may use text. |
| other keys | any | Preserved. |

### Resolution order

For a service reference, the UI builds an ordered key list:

1. composite key: `key:<protocol>/<transport>/<entry>`;
2. alias key: `alias:<alias>`.

The composite key wins. This means a mapping follows a renamed alias as long as
the protocol/transport/entry identity stays the same. If the composite key has
no entry, the alias key is used.

Numeric service IDs in the range `0..=255` also work as Z-Wave node IDs in the
builder's node picker and message views. Unknown IDs fall back to the raw node
number; they are never invented as labels.

### Editing tips

- Keep device IDs stable; they are join keys, not display text.
- Prefer composite service keys for mappings that should survive alias renames.
- Duplicate `device_id` values in `devices` are not expected; keep them unique.
- The bundled file is demo/fixture data. A real deployment can replace it next
  to the executable or in the web bundle.
- Invalid JSON keeps the embedded catalog active.
