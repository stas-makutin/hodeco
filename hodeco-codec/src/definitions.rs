//! Static command definitions bundled with the UI.
//!
//! Definitions are read at compile time. Editing a file requires rebuilding the
//! application; that is intentional and documented in the UX notes.
//!
//! The schema is intentionally small but expressive enough to drive the Parsed
//! decoder. The builder's typed helper subset shares these command IDs and field
//! descriptors:
//!
//! - control frame and data-frame metadata;
//! - Serial API request/response/callback layouts;
//! - command-class and command field descriptors;
//! - field types, enum values, display formats, and length/count references.

use serde::Deserialize;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZWaveDefinitions {
    pub version: u32,
    pub protocol: String,
    #[serde(default)]
    pub control_frames: Vec<ControlFrameDef>,
    #[serde(default)]
    pub frame_types: Vec<FrameTypeDef>,
    #[serde(default)]
    pub serial_commands: Vec<SerialCommandDef>,
    #[serde(default)]
    pub command_classes: Vec<CommandClassDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFrameDef {
    pub id: u8,
    pub name: String,
    pub kind: ControlFrameKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFrameKind {
    Ack,
    Nak,
    Can,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTypeDef {
    pub id: u8,
    pub name: String,
    pub direction: FrameDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameDirection {
    Request,
    Response,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialCommandDef {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Operation-picker sort priority. Lower values appear first.
    #[serde(default = "default_serial_priority")]
    pub priority: i32,
    /// Builder editor selected for this command. `generic` uses `fields`.
    #[serde(default)]
    pub builder: SerialBuilderKind,
    /// Incoming notifications are decoded but cannot be sent as requests.
    #[serde(default)]
    pub incoming_only: bool,
    /// Selected when the Send message dialog opens for this protocol.
    #[serde(default)]
    pub default_operation: bool,
    /// Highlight the operation in the Operation picker.
    #[serde(default)]
    pub emphasize: bool,
    /// Request fields. These are also the builder-facing field descriptors.
    #[serde(default)]
    pub fields: Vec<LayoutField>,
    /// The command takes no request parameters. This is different from an
    /// empty `fields` list, which means no field layout is available yet.
    #[serde(default)]
    pub no_request_parameters: bool,
    #[serde(default)]
    pub request_summary: Option<String>,
    #[serde(default)]
    pub response_fields: Vec<LayoutField>,
    #[serde(default)]
    pub response_summary: Option<String>,
    /// Some Serial API responses are callback frames with a different layout.
    #[serde(default)]
    pub callback: Option<SerialCallbackDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialCallbackDef {
    /// Exact parameter-byte length that selects this layout.
    pub data_length: usize,
    #[serde(default)]
    pub fields: Vec<LayoutField>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SerialBuilderKind {
    #[default]
    Generic,
    SendData,
    SendDataMulti,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandClassDef {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub commands: Vec<CommandDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDef {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Vec<LayoutField>,
    /// Optional one-line detail template, for example `"{value}"`.
    #[serde(default)]
    pub summary: Option<String>,
}

/// A field layout entry: either a normal field or a packed group of fields
/// sharing one big-endian integer value.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LayoutField {
    Field(Box<FieldDef>),
    Group(PackedGroupDef),
}

impl LayoutField {
    pub fn field(&self) -> Option<&FieldDef> {
        match self {
            Self::Field(field) => Some(field),
            Self::Group(_) => None,
        }
    }

    pub fn fields(&self) -> std::slice::Iter<'_, FieldDef> {
        match self {
            Self::Field(field) => std::slice::from_ref(field.as_ref()).iter(),
            Self::Group(group) => group.fields.iter(),
        }
    }
}

/// A group of fields packed into a fixed-width big-endian integer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackedGroupDef {
    /// Diagnostic name; not rendered.
    pub group: String,
    /// Width of the packed value in bytes.
    pub length: usize,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    pub name: String,
    pub label: String,
    pub kind: FieldType,
    #[serde(default)]
    pub description: Option<String>,
    /// Initial builder value, parsed according to the field kind. Useful for
    /// protocol defaults such as the ZigBee HA profile.
    #[serde(default, rename = "default")]
    pub default_value: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub values: Vec<EnumValue>,
    /// Bit mask applied before looking up an enum value, or within a packed
    /// group field.
    #[serde(default)]
    pub mask: Option<u64>,
    /// Right-shift applied after `mask` within a packed group field.
    #[serde(default)]
    pub shift: u8,
    /// Append the raw byte value to enum and flag labels.
    #[serde(default)]
    pub raw_suffix: bool,
    /// Text used when no enum value or flag matches.
    #[serde(default)]
    pub unknown: Option<String>,
    /// Semantic role for address extraction, field colouring, and protocol UI
    /// capabilities.
    #[serde(default)]
    pub role: FieldRole,
    /// Optional payload-filter slot. The ZigBee filter uses this instead of
    /// deriving categories from field names.
    #[serde(default)]
    pub filter_slot: Option<FilterSlot>,
    /// How a decoded value is rendered. `default` renders the natural value.
    #[serde(default)]
    pub format: FieldFormat,
    /// Fixed length in bytes for variable-width fields such as text and hex.
    #[serde(default)]
    pub length: Option<usize>,
    /// Length in bytes comes from an earlier numeric field.
    #[serde(default)]
    pub length_from: Option<String>,
    /// Element count comes from an earlier numeric field.
    #[serde(default)]
    pub count_from: Option<String>,
    /// Consume all remaining bytes.
    #[serde(default)]
    pub remaining: bool,
    /// The field is derived from another field and is not a builder input.
    #[serde(default)]
    pub derived: bool,
    /// Multi-byte integer byte order. Z-Wave defaults to big-endian; EZSP and
    /// ZCL use little-endian.
    #[serde(default)]
    pub endian: Endian,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endian {
    #[default]
    Big,
    Little,
}

impl Endian {
    pub fn read_unsigned(self, bytes: &[u8]) -> u64 {
        match self {
            Self::Big => bytes
                .iter()
                .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)),
            Self::Little => bytes
                .iter()
                .rev()
                .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)),
        }
    }

    pub fn read_signed(self, bytes: &[u8]) -> i64 {
        match bytes.len() {
            0 => 0,
            1 => i64::from(bytes[0] as i8),
            2 => {
                let value = self.read_unsigned(bytes) as u16;
                i64::from(value as i16)
            }
            4 => {
                let value = self.read_unsigned(bytes) as u32;
                i64::from(value as i32)
            }
            _ => self.read_unsigned(bytes) as i64,
        }
    }

    pub fn encode_unsigned(self, value: u64, width: usize) -> Vec<u8> {
        let mut bytes = value.to_be_bytes().to_vec();
        match self {
            Self::Big => bytes.split_off(8 - width.min(8)),
            Self::Little => {
                let mut bytes = bytes.split_off(8 - width.min(8));
                bytes.reverse();
                bytes
            }
        }
    }

    pub fn encode_signed(self, value: i64, width: usize) -> Vec<u8> {
        self.encode_unsigned(value as u64, width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    Node,
    NodeList,
    Uint8,
    Uint16,
    Uint32,
    Bool,
    Enum,
    Flags,
    Hex,
    Duration,
    Text,
    TxOptions,
    CallbackId,
    CommandClassData,
    /// Signed integer. Width normally comes from `lengthFrom`.
    Int,
    /// Unsigned integer. Width normally comes from `lengthFrom`.
    Uint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterSlot {
    Network,
    Group,
    Cluster,
    Endpoint,
    Payload,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldRole {
    #[default]
    Value,
    Length,
    Target,
    Targets,
    Broadcast,
    Source,
    CommandClass,
    Cluster,
    Profile,
    Group,
    SourceEndpoint,
    DestinationEndpoint,
    Payload,
    DestinationType,
    RxStatus,
    TxStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldFormat {
    #[default]
    Default,
    Hex,
    Decimal,
    Level,
    Duration,
    MeterRateScale,
    TxOptions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumValue {
    pub value: u8,
    pub name: String,
    /// This enum value enables the known-device picker for a destination
    /// type field.
    #[serde(default)]
    pub direct: bool,
    /// Wire length in bytes for enum values that select a field width.
    #[serde(default)]
    pub length: Option<usize>,
}

fn default_true() -> bool {
    true
}

fn default_serial_priority() -> i32 {
    1000
}

impl ZWaveDefinitions {
    pub fn control_frame(&self, id: u8) -> Option<&ControlFrameDef> {
        self.control_frames.iter().find(|frame| frame.id == id)
    }

    pub fn frame_type(&self, id: u8) -> Option<&FrameTypeDef> {
        self.frame_types
            .iter()
            .find(|frame_type| frame_type.id == id)
    }

    pub fn serial_command(&self, id: u8) -> Option<&SerialCommandDef> {
        self.serial_commands.iter().find(|command| command.id == id)
    }

    pub fn command_class(&self, id: u8) -> Option<&CommandClassDef> {
        self.command_classes.iter().find(|class| class.id == id)
    }

    pub fn command(&self, class: u8, command: u8) -> Option<&CommandDef> {
        self.command_class(class)?
            .commands
            .iter()
            .find(|definition| definition.id == command)
    }
}

impl FieldType {
    /// Fixed width in bytes, or `None` for length/count/remaining-driven fields.
    pub fn fixed_width(self) -> Option<usize> {
        match self {
            Self::Node
            | Self::Uint8
            | Self::Bool
            | Self::Enum
            | Self::Flags
            | Self::Duration
            | Self::TxOptions
            | Self::CallbackId => Some(1),
            Self::Uint16 => Some(2),
            Self::Uint32 => Some(4),
            Self::NodeList
            | Self::Hex
            | Self::Text
            | Self::CommandClassData
            | Self::Int
            | Self::Uint => None,
        }
    }

    /// Numeric fields can be referenced by `lengthFrom` and `countFrom`.
    #[cfg(test)]
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::Uint8
                | Self::Uint16
                | Self::Uint32
                | Self::Node
                | Self::Bool
                | Self::Enum
                | Self::Flags
                | Self::Duration
                | Self::TxOptions
                | Self::CallbackId
                | Self::Int
                | Self::Uint
        )
    }
}

static INSTALLED: OnceLock<RwLock<Option<Arc<ZWaveDefinitions>>>> = OnceLock::new();

fn installed() -> &'static RwLock<Option<Arc<ZWaveDefinitions>>> {
    INSTALLED.get_or_init(|| RwLock::new(None))
}

/// Parsed bundled `data/zwave.json`.
///
/// This is the compile-time fallback used when no external definition file was
/// installed at startup.
pub fn bundled() -> &'static ZWaveDefinitions {
    static DEFINITIONS: OnceLock<ZWaveDefinitions> = OnceLock::new();
    DEFINITIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/zwave.json"))
            .expect("bundled zwave.json must be valid")
    })
}

fn bundled_shared() -> Arc<ZWaveDefinitions> {
    static SHARED: OnceLock<Arc<ZWaveDefinitions>> = OnceLock::new();
    Arc::clone(SHARED.get_or_init(|| Arc::new(bundled().clone())))
}

/// Installs an externally loaded definition file.
///
/// The UI calls this once at startup after reading `zwave.json` next to the
/// executable or the web bundle. Invalid JSON is rejected and the embedded
/// fallback stays active.
pub fn install(json: &str) -> Result<(), String> {
    let definitions: ZWaveDefinitions = serde_json::from_str(json)
        .map_err(|error| format!("invalid Z-Wave definitions: {error}"))?;
    if definitions.protocol != "zwave" {
        return Err(format!(
            "invalid Z-Wave definitions: expected protocol 'zwave', found '{}'",
            definitions.protocol
        ));
    }
    *installed()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(definitions));
    Ok(())
}

/// The definitions currently in use: the installed file when present, or the
/// embedded fallback.
pub fn current() -> Arc<ZWaveDefinitions> {
    installed()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(bundled_shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn bundled_definitions_parse() {
        let definitions = bundled();
        assert_eq!(definitions.version, 1);
        assert_eq!(definitions.protocol, "zwave");
        assert!(definitions.serial_command(0x15).is_some());
        assert!(definitions.serial_command(0x13).is_some());
        assert!(definitions.serial_command(0x14).is_some());
        assert!(definitions.control_frame(0x06).is_some());
        assert!(definitions.frame_type(0x00).is_some());
    }

    #[test]
    fn contains_the_supported_command_class_subset() {
        let definitions = bundled();
        for id in [0x20, 0x25, 0x26, 0x2b, 0x32] {
            assert!(
                definitions.command_class(id).is_some(),
                "missing command class 0x{id:02x}"
            );
        }
        assert_eq!(
            definitions
                .command(0x20, 0x01)
                .map(|command| command.name.as_str()),
            Some("Set")
        );
    }

    #[test]
    fn contains_the_extended_serial_and_command_catalog() {
        let definitions = bundled();

        for id in [0x02, 0x05, 0x07, 0x09, 0x41, 0x60, 0xBD] {
            assert!(
                definitions.serial_command(id).is_some(),
                "missing serial command 0x{id:02x}"
            );
        }

        for id in [
            0x30, 0x31, 0x40, 0x43, 0x44, 0x5B, 0x5E, 0x62, 0x70, 0x71, 0x72, 0x80, 0x84, 0x85,
            0x86, 0x9C,
        ] {
            assert!(
                definitions.command_class(id).is_some(),
                "missing command class 0x{id:02x}"
            );
        }

        for (class, command) in [
            (0x40, 0x01),
            (0x43, 0x03),
            (0x70, 0x06),
            (0x71, 0x05),
            (0x72, 0x05),
            (0x86, 0x12),
        ] {
            assert!(
                definitions.command(class, command).is_some(),
                "missing command 0x{class:02x}/0x{command:02x}"
            );
        }
    }

    #[test]
    fn covers_the_documented_serial_and_command_subset() {
        let definitions = bundled();
        for id in [0x04, 0x05, 0x0a, 0x13, 0x14, 0x15, 0x20] {
            assert!(
                definitions.serial_command(id).is_some(),
                "missing serial command 0x{id:02x}"
            );
        }
        for (class, command) in [
            (0x20, 0x01),
            (0x20, 0x02),
            (0x20, 0x03),
            (0x25, 0x01),
            (0x25, 0x02),
            (0x25, 0x03),
            (0x26, 0x01),
            (0x26, 0x02),
            (0x26, 0x03),
            (0x26, 0x04),
            (0x26, 0x05),
            (0x2b, 0x01),
            (0x32, 0x01),
            (0x32, 0x02),
            (0x32, 0x03),
            (0x32, 0x04),
            (0x32, 0x05),
        ] {
            assert!(
                definitions.command(class, command).is_some(),
                "missing command 0x{class:02x}/0x{command:02x}"
            );
        }
    }

    #[test]
    fn configuration_and_setback_values_use_integer_types() {
        let definitions = bundled();
        for (command_id, field_name) in [(0x04, "value"), (0x06, "value")] {
            let command = definitions.command(0x70, command_id).unwrap();
            assert!(command.fields.iter().any(|item| {
                matches!(
                    item,
                    LayoutField::Field(field)
                        if field.name == field_name
                            && field.kind == FieldType::Int
                            && field.length_from.as_deref() == Some("size")
                )
            }));
        }

        let properties = definitions.command(0x70, 0x0F).unwrap();
        for name in ["minValue", "maxValue", "defaultValue"] {
            assert!(properties.fields.iter().any(|item| {
                matches!(
                    item,
                    LayoutField::Field(field)
                        if field.name == name && field.kind == FieldType::Int
                )
            }));
        }

        let setback = definitions.command(0x47, 0x01).unwrap();
        assert!(setback.fields.iter().any(|item| {
            matches!(
                item,
                LayoutField::Field(field)
                    if field.name == "value"
                        && field.kind == FieldType::Int
                        && field.length == Some(1)
            )
        }));
    }

    #[test]
    fn serial_field_references_resolve() {
        let definitions = bundled();
        for command in &definitions.serial_commands {
            let fields: Vec<&FieldDef> = command
                .fields
                .iter()
                .flat_map(LayoutField::fields)
                .collect();
            let names: BTreeSet<&str> = fields.iter().map(|field| field.name.as_str()).collect();
            assert_eq!(
                names.len(),
                fields.len(),
                "duplicate field name in serial command 0x{:02x}",
                command.id
            );
            if command.no_request_parameters {
                assert!(
                    command.fields.is_empty(),
                    "serial command 0x{:02x} has both noRequestParameters and fields",
                    command.id
                );
            }
            for field in fields {
                for reference in [&field.length_from, &field.count_from]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        names.contains(reference.as_str()),
                        "serial command 0x{:02x} field '{}' references missing field '{}'",
                        command.id,
                        field.name,
                        reference
                    );
                }
                for (reference, kind) in [
                    (&field.length_from, "lengthFrom"),
                    (&field.count_from, "countFrom"),
                ] {
                    if let Some(target) = reference {
                        let target = command
                            .fields
                            .iter()
                            .flat_map(LayoutField::fields)
                            .find(|candidate| candidate.name == *target)
                            .expect("reference was checked above");
                        assert!(
                            target.kind.fixed_width().is_some(),
                            "serial command 0x{:02x} field '{}' {kind} target '{}' is not numeric",
                            command.id,
                            field.name,
                            target.name
                        );
                    }
                }
            }
        }

        let node_info = definitions.serial_command(0x03).unwrap();
        assert!(!node_info.no_request_parameters);
        assert_eq!(node_info.fields.len(), 5);
        assert!(
            definitions
                .serial_command(0x05)
                .unwrap()
                .no_request_parameters
        );

        assert_eq!(definitions.serial_command(0x13).unwrap().priority, 10);
        assert_eq!(definitions.serial_command(0x14).unwrap().priority, 20);
        assert_eq!(definitions.serial_command(0x15).unwrap().priority, 1000);
        assert_eq!(definitions.serial_command(0x02).unwrap().priority, 1000);

        assert_eq!(
            definitions.serial_command(0x13).unwrap().builder,
            SerialBuilderKind::SendData
        );
        assert_eq!(
            definitions.serial_command(0x14).unwrap().builder,
            SerialBuilderKind::SendDataMulti
        );
        assert_eq!(
            definitions.serial_command(0x15).unwrap().builder,
            SerialBuilderKind::Generic
        );
        assert!(definitions.serial_command(0x13).unwrap().default_operation);
        assert!(definitions.serial_command(0x13).unwrap().emphasize);
        assert!(definitions.serial_command(0x14).unwrap().emphasize);
        assert!(!definitions.serial_command(0x15).unwrap().emphasize);
        assert!(definitions.serial_command(0x04).unwrap().incoming_only);
    }

    #[test]
    fn ids_and_names_are_unique() {
        let definitions = bundled();
        let mut serial_ids = BTreeSet::new();
        for command in &definitions.serial_commands {
            assert!(
                serial_ids.insert(command.id),
                "duplicate serial command id 0x{:02x}",
                command.id
            );
        }

        let mut class_ids = BTreeSet::new();
        for class in &definitions.command_classes {
            assert!(
                class_ids.insert(class.id),
                "duplicate command class id 0x{:02x}",
                class.id
            );
            let mut command_ids = BTreeSet::new();
            for command in &class.commands {
                assert!(
                    command_ids.insert(command.id),
                    "duplicate command 0x{:02x}/0x{:02x}",
                    class.id,
                    command.id
                );
            }
        }

        let mut control_ids = BTreeSet::new();
        for frame in &definitions.control_frames {
            assert!(control_ids.insert(frame.id), "duplicate control frame");
        }

        let mut frame_type_ids = BTreeSet::new();
        for frame in &definitions.frame_types {
            assert!(frame_type_ids.insert(frame.id), "duplicate frame type");
        }
    }

    #[test]
    fn z_wave_display_names_are_human_readable() {
        let definitions = bundled();
        assert_eq!(
            definitions.command_class(0x25).unwrap().name,
            "Switch Binary"
        );
        assert_eq!(
            definitions.command_class(0x26).unwrap().name,
            "Switch Multilevel"
        );
        assert_eq!(
            definitions.command_class(0x2B).unwrap().name,
            "Scene Activation"
        );
        assert_eq!(
            definitions.command(0x26, 0x04).unwrap().name,
            "Start Level Change"
        );
        assert_eq!(
            definitions.command(0x26, 0x05).unwrap().name,
            "Stop Level Change"
        );
        assert_eq!(
            definitions.command(0x32, 0x03).unwrap().name,
            "Supported Get"
        );
        assert_eq!(
            definitions.command(0x32, 0x04).unwrap().name,
            "Supported Report"
        );
    }

    #[test]
    fn command_field_values_and_defaults_are_consistent() {
        let definitions = bundled();
        for command in &definitions.serial_commands {
            check_field_values(&command.name, &command.fields);
            check_field_values(
                &format!("{} response", command.name),
                &command.response_fields,
            );
            if let Some(callback) = &command.callback {
                check_field_values(&format!("{} callback", command.name), &callback.fields);
            }
        }
        for class in &definitions.command_classes {
            for command in &class.commands {
                check_field_values(&format!("{} {}", class.name, command.name), &command.fields);
            }
        }
    }

    fn check_field_values(context: &str, layout: &[LayoutField]) {
        for field in layout.iter().flat_map(LayoutField::fields) {
            if field.values.is_empty() {
                continue;
            }
            let mut values = BTreeSet::new();
            let mut names = BTreeSet::new();
            for value in &field.values {
                assert!(
                    values.insert(value.value),
                    "{context}: field {} has duplicate value 0x{:02x}",
                    field.name,
                    value.value
                );
                assert!(
                    !value.name.trim().is_empty(),
                    "{context}: field {} has an empty value name",
                    field.name
                );
                assert!(
                    names.insert(value.name.as_str()),
                    "{context}: field {} has duplicate value name '{}'",
                    field.name,
                    value.name
                );
            }
            if let Some(default) = &field.default_value {
                let default = default.trim();
                let number = default
                    .strip_prefix("0x")
                    .and_then(|text| u8::from_str_radix(text, 16).ok())
                    .or_else(|| default.parse::<u8>().ok());
                assert!(
                    field.values.iter().any(|value| value.name == default)
                        || number.is_some_and(|number| {
                            field.values.iter().any(|value| value.value == number)
                        }),
                    "{context}: field {} default '{default}' is not one of its values",
                    field.name
                );
            }
        }
    }

    #[test]
    fn field_descriptors_reference_earlier_numeric_fields() {
        let definitions = bundled();
        for command in &definitions.serial_commands {
            validate_layout(&format!("serial command {}", command.name), &command.fields);
            validate_layout(
                &format!("{} response", command.name),
                &command.response_fields,
            );
            if let Some(callback) = &command.callback {
                validate_layout(&format!("{} callback", command.name), &callback.fields);
            }
        }
        for class in &definitions.command_classes {
            for command in &class.commands {
                validate_layout(&format!("{} {}", class.name, command.name), &command.fields);
            }
        }
    }

    fn validate_layout(context: &str, layout: &[LayoutField]) {
        let mut names = BTreeSet::new();
        let mut numeric = BTreeSet::new();
        let mut fields = Vec::new();
        let mut groups = BTreeSet::new();
        for (index, item) in layout.iter().enumerate() {
            match item {
                LayoutField::Field(field) => {
                    fields.push(field.as_ref());
                    if field.remaining {
                        assert_eq!(
                            index,
                            layout.len() - 1,
                            "{context}: remaining field {} must be last",
                            field.name
                        );
                    }
                }
                LayoutField::Group(group) => {
                    assert!(
                        groups.insert(group.group.as_str()),
                        "{context}: duplicate group {}",
                        group.group
                    );
                    assert!(
                        (1..=8).contains(&group.length),
                        "{context}: group {} has unsupported length {}",
                        group.group,
                        group.length
                    );
                    assert!(
                        !group.fields.is_empty(),
                        "{context}: group {} has no fields",
                        group.group
                    );
                    let mut used_mask = 0u64;
                    for field in &group.fields {
                        fields.push(field);
                        assert!(
                            !field.remaining,
                            "{context}: packed field {} cannot use remaining",
                            field.name
                        );
                        assert!(
                            field.length_from.is_none() && field.count_from.is_none(),
                            "{context}: packed field {} cannot use length/count references",
                            field.name
                        );
                        let mask = field.mask.expect("packed field must have a mask");
                        assert!(
                            mask != 0,
                            "{context}: packed field {} has zero mask",
                            field.name
                        );
                        let width = mask.count_ones() as usize;
                        assert!(
                            field.shift as usize + width <= group.length * 8,
                            "{context}: packed field {} exceeds group width",
                            field.name
                        );
                        let expected = (u64::MAX >> (64 - width)) << field.shift;
                        assert_eq!(
                            mask, expected,
                            "{context}: packed field {} mask must be contiguous and match shift",
                            field.name
                        );
                        assert_eq!(
                            used_mask & mask,
                            0,
                            "{context}: packed field {} overlaps another field",
                            field.name
                        );
                        used_mask |= mask;
                    }
                }
            }
        }

        for field in &fields {
            assert!(
                names.insert(field.name.as_str()),
                "{context}: duplicate field name {}",
                field.name
            );
            if field.remaining {
                assert!(
                    !field.derived,
                    "{context}: remaining field {} cannot be derived",
                    field.name
                );
            }
            if let Some(source) = &field.length_from {
                assert!(
                    numeric.contains(source.as_str()),
                    "{context}: {} references unknown length field {source}",
                    field.name
                );
                assert!(
                    field.kind.fixed_width().is_none(),
                    "{context}: fixed-width field {} cannot use lengthFrom",
                    field.name
                );
            }
            if let Some(source) = &field.count_from {
                assert!(
                    numeric.contains(source.as_str()),
                    "{context}: {} references unknown count field {source}",
                    field.name
                );
                assert_eq!(
                    field.kind,
                    FieldType::NodeList,
                    "{context}: countFrom is only valid for nodeList fields"
                );
            }
            if matches!(field.kind, FieldType::Enum | FieldType::Flags) {
                assert!(
                    !field.values.is_empty(),
                    "{context}: enum/flags field {} has no values",
                    field.name
                );
                let mut values = BTreeSet::new();
                let mut lengths = BTreeSet::new();
                for value in &field.values {
                    assert!(
                        values.insert(value.value),
                        "{context}: enum field {} has duplicate value {}",
                        field.name,
                        value.value
                    );
                    if let Some(length) = value.length {
                        assert!(
                            length > 0,
                            "{context}: enum field {} has zero length",
                            field.name
                        );
                        assert!(
                            lengths.insert(length),
                            "{context}: enum field {} has duplicate length {length}",
                            field.name
                        );
                    }
                }
            }
            if let (Some(min), Some(max)) = (field.min, field.max) {
                assert!(min <= max, "{context}: {} has min > max", field.name);
            }
            if field.kind.is_numeric() {
                numeric.insert(field.name.as_str());
            }
        }
    }
}
