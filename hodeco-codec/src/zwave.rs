//! Z-Wave Serial API frame decoding.
//!
//! Frame grammar (control bytes, length, checksum) is handled here. Serial API
//! commands and command-class payloads are decoded from the bundled
//! [`definitions`](crate::definitions) table, so adding a command or field is a
//! data change rather than a decoder change.
//!
//! The decoder is deliberately tolerant: a payload may contain ACK/NAK bytes,
//! multiple concatenated frames, an incomplete frame, or arbitrary bytes. It
//! never panics on malformed input and always reports the raw ranges it saw.

use crate::definitions::{
    self, FieldDef, FieldFormat, FieldRole, FieldType, FrameDirection, LayoutField, PackedGroupDef,
    SerialCommandDef, ZWaveDefinitions,
};
use crate::hex::spaced_hex;
use crate::parsed::{DecodeResult, DecodedField, DecodedFrame, FieldKind, FrameAddress, FrameKind};
use std::collections::BTreeMap;

pub const FRAME_SOF: u8 = 0x01;
pub const FRAME_ACK: u8 = 0x06;
pub const FRAME_NAK: u8 = 0x15;
pub const FRAME_CAN: u8 = 0x18;

pub const FRAME_REQUEST: u8 = 0x00;
pub const FRAME_RESPONSE: u8 = 0x01;

pub const FRAME_MIN_LENGTH: usize = 3;
pub const FRAME_MAX_LENGTH: usize = 253;
pub const MAX_FRAME_BYTES: usize = FRAME_MAX_LENGTH + 2;

pub const SERIAL_APPLICATION_COMMAND_HANDLER: u8 = 0x04;
pub const SERIAL_SEND_DATA: u8 = 0x13;
pub const SERIAL_SEND_DATA_MULTI: u8 = 0x14;
pub const SERIAL_GET_VERSION: u8 = 0x15;

pub const CC_BASIC: u8 = 0x20;
pub const CC_SWITCH_BINARY: u8 = 0x25;
pub const CC_SWITCH_MULTILEVEL: u8 = 0x26;
pub const CC_SCENE_ACTIVATION: u8 = 0x2b;
pub const CC_METER: u8 = 0x32;

/// Z-Wave checksum: XOR of the supplied bytes, starting from `0xff`.
pub fn checksum(source: &[u8]) -> u8 {
    source.iter().fold(0xff, |result, byte| result ^ byte)
}

/// Builds a complete Serial API frame. `body` is the serial command followed by
/// its data bytes.
pub fn build_frame(frame_type: u8, body: &[u8]) -> Vec<u8> {
    let length =
        u8::try_from(body.len() + 2).expect("z-wave frame body must fit into a single length byte");
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.push(length);
    frame.push(frame_type);
    frame.extend_from_slice(body);
    let checksum = checksum(&frame);
    frame.push(checksum);
    frame.insert(0, FRAME_SOF);
    frame
}

pub fn request(body: &[u8]) -> Vec<u8> {
    build_frame(FRAME_REQUEST, body)
}

pub fn response(body: &[u8]) -> Vec<u8> {
    build_frame(FRAME_RESPONSE, body)
}

pub fn serial_command_name(command: u8) -> Option<String> {
    let definitions = definitions::current();
    definitions
        .serial_command(command)
        .map(|definition| definition.name.clone())
}

#[cfg(test)]
pub fn command_class_name(class: u8) -> Option<String> {
    let definitions = definitions::current();
    definitions
        .command_class(class)
        .map(|definition| definition.name.clone())
}

/// Decodes a Z-Wave payload. The result is intended for display and never
/// fails; malformed sections are reported through `issues`.
pub fn decode(payload: &[u8]) -> DecodeResult {
    let definitions = definitions::current();
    let mut result = DecodeResult::default();
    let mut offset = 0;

    while offset < payload.len() {
        if payload[offset] == FRAME_SOF {
            if payload.len() - offset < 2 {
                result
                    .issues
                    .push(format!("incomplete frame header at offset 0x{offset:04x}"));
                result
                    .frames
                    .push(raw_frame(payload, offset, payload.len()));
                break;
            }

            let frame_length = payload[offset + 1] as usize;
            if !(FRAME_MIN_LENGTH..=FRAME_MAX_LENGTH).contains(&frame_length) {
                result.issues.push(format!(
                    "invalid frame length {frame_length} at offset 0x{offset:04x}"
                ));
                result.frames.push(raw_frame(payload, offset, offset + 1));
                offset += 1;
                continue;
            }

            let total = frame_length + 2;
            if payload.len() - offset < total {
                result.issues.push(format!(
                    "incomplete frame at offset 0x{offset:04x}: expected {total} bytes, found {}",
                    payload.len() - offset
                ));
                result
                    .frames
                    .push(raw_frame(payload, offset, payload.len()));
                break;
            }

            let frame_bytes = &payload[offset..offset + total];
            let checksum_index = total - 1;
            let expected = checksum(&frame_bytes[1..checksum_index]);
            let actual = frame_bytes[checksum_index];
            let checksum_ok = expected == actual;
            if !checksum_ok {
                result.issues.push(format!(
                    "invalid checksum at offset 0x{offset:04x}: expected 0x{expected:02x}, got 0x{actual:02x}"
                ));
            }
            result.frames.push(decode_data_frame(
                &definitions,
                offset,
                frame_bytes,
                checksum_ok,
                &mut result.issues,
            ));
            offset += total;
            continue;
        }

        if let Some(control) = definitions.control_frame(payload[offset]) {
            result
                .frames
                .push(control_frame(offset, control.name.as_str(), control.kind));
            offset += 1;
            continue;
        }

        let start = offset;
        while offset < payload.len() && !is_frame_marker(&definitions, payload[offset]) {
            offset += 1;
        }
        let skipped = offset - start;
        result.issues.push(format!(
            "unrecognised {skipped} byte(s) at offset 0x{start:04x}"
        ));
        result.frames.push(raw_frame(payload, start, offset));
    }

    result.summary = if result.frames.is_empty() {
        if payload.is_empty() {
            "Empty payload".to_string()
        } else {
            format!("Raw payload · {} bytes", payload.len())
        }
    } else {
        result
            .frames
            .iter()
            .map(|frame| frame.summary.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    result
}

fn is_frame_marker(definitions: &ZWaveDefinitions, byte: u8) -> bool {
    byte == FRAME_SOF || definitions.control_frame(byte).is_some()
}

fn control_frame(offset: usize, name: &str, kind: definitions::ControlFrameKind) -> DecodedFrame {
    let kind = match kind {
        definitions::ControlFrameKind::Ack => FrameKind::Ack,
        definitions::ControlFrameKind::Nak => FrameKind::Nak,
        definitions::ControlFrameKind::Can => FrameKind::Can,
    };
    DecodedFrame {
        kind,
        range: offset..offset + 1,
        title: name.to_string(),
        summary: name.to_string(),
        checksum_ok: None,
        fields: vec![DecodedField {
            label: "Control".to_string(),
            value: name.to_string(),
            kind: FieldKind::Control,
            range: offset..offset + 1,
        }],
        address: FrameAddress::default(),
    }
}

fn raw_frame(payload: &[u8], start: usize, end: usize) -> DecodedFrame {
    let end = end.min(payload.len());
    DecodedFrame {
        kind: FrameKind::Unknown,
        range: start..end,
        title: "Unrecognised bytes".to_string(),
        summary: format!("Unrecognised bytes · {} byte(s)", end.saturating_sub(start)),
        checksum_ok: None,
        fields: vec![DecodedField {
            label: "Data".to_string(),
            value: spaced_hex(&payload[start..end]),
            kind: FieldKind::Value,
            range: start..end,
        }],
        address: FrameAddress::default(),
    }
}

fn decode_data_frame(
    definitions: &ZWaveDefinitions,
    offset: usize,
    frame: &[u8],
    checksum_ok: bool,
    issues: &mut Vec<String>,
) -> DecodedFrame {
    let frame_length = frame[1] as usize;
    let frame_type = frame[2];
    let body = &frame[3..frame.len() - 1];
    let command = body.first().copied().unwrap_or_default();
    let data = body.get(1..).unwrap_or_default();
    let command_definition = definitions.serial_command(command);
    let command_name = command_definition
        .map(|definition| definition.name.clone())
        .unwrap_or_else(|| format!("0x{command:02x}"));
    let type_name = frame_type_name(definitions, frame_type);
    let mut meta = DecodeMeta::default();

    let mut fields = vec![
        DecodedField {
            label: "SOF".to_string(),
            value: "0x01".to_string(),
            kind: FieldKind::Header,
            range: offset..offset + 1,
        },
        DecodedField {
            label: "Length".to_string(),
            value: frame_length.to_string(),
            kind: FieldKind::Length,
            range: offset + 1..offset + 2,
        },
        DecodedField {
            label: "Type".to_string(),
            value: format!("{type_name} (0x{frame_type:02x})"),
            kind: FieldKind::FrameType,
            range: offset + 2..offset + 3,
        },
        DecodedField {
            label: "Command".to_string(),
            value: format!("{command_name} (0x{command:02x})"),
            kind: FieldKind::Command,
            range: offset + 3..offset + 4,
        },
    ];

    let detail = decode_serial_command(
        definitions,
        &mut fields,
        command_definition,
        frame_type,
        data,
        offset + 4,
        &mut meta,
        issues,
    );

    fields.push(DecodedField {
        label: "Checksum".to_string(),
        value: if checksum_ok {
            format!("0x{:02x} (valid)", frame[frame.len() - 1])
        } else {
            format!("0x{:02x} (invalid)", frame[frame.len() - 1])
        },
        kind: FieldKind::Checksum,
        range: offset + frame.len() - 1..offset + frame.len(),
    });

    let title = format!("{command_name} · {type_name}");
    DecodedFrame {
        kind: FrameKind::Data,
        range: offset..offset + frame.len(),
        summary: if detail.is_empty() {
            title.clone()
        } else {
            detail
        },
        title,
        checksum_ok: Some(checksum_ok),
        fields,
        address: FrameAddress {
            targets: meta.targets,
            source: meta.source,
            command_class: meta.command_class,
            command: meta.command,
        },
    }
}

/// Routing and command metadata populated while parsing fields.
#[derive(Debug, Default)]
struct DecodeMeta {
    targets: Vec<u8>,
    source: Option<u8>,
    command_class: Option<u8>,
    command: Option<u8>,
}

#[allow(clippy::too_many_arguments)]
fn decode_serial_command(
    definitions: &ZWaveDefinitions,
    fields: &mut Vec<DecodedField>,
    command_definition: Option<&SerialCommandDef>,
    frame_type: u8,
    data: &[u8],
    data_offset: usize,
    meta: &mut DecodeMeta,
    issues: &mut Vec<String>,
) -> String {
    let command_name = command_definition
        .map(|definition| definition.name.as_str())
        .unwrap_or("unknown command");
    let direction = definitions
        .frame_type(frame_type)
        .map(|definition| definition.direction);

    let Some(command_definition) = command_definition else {
        add_raw_data(fields, "Data", data, data_offset);
        return serial_fallback_summary(command_name, direction.map(LayoutKind::from));
    };

    let (layout, summary, kind) = match direction {
        Some(FrameDirection::Request) => {
            if let Some(callback) = &command_definition.callback
                && data.len() == callback.data_length
            {
                (
                    callback.fields.as_slice(),
                    callback.summary.as_deref(),
                    LayoutKind::Callback,
                )
            } else {
                (
                    command_definition.fields.as_slice(),
                    command_definition.request_summary.as_deref(),
                    LayoutKind::Request,
                )
            }
        }
        Some(FrameDirection::Response) => (
            command_definition.response_fields.as_slice(),
            command_definition.response_summary.as_deref(),
            LayoutKind::Response,
        ),
        None => {
            add_raw_data(fields, "Data", data, data_offset);
            return command_name.to_string();
        }
    };

    let context = format!("{} {kind}", command_definition.name);
    let parsed = decode_field_layout(
        definitions,
        fields,
        layout,
        data,
        data_offset,
        meta,
        issues,
        &context,
    );

    if layout.is_empty() && !data.is_empty() {
        add_raw_data(fields, "Data", data, data_offset);
    }

    if let Some(template) = summary {
        let rendered = render_template(template, &parsed.values, parsed.nested_summary.as_deref());
        if !rendered.is_empty() {
            return rendered;
        }
    }
    serial_fallback_summary(command_definition.name.as_str(), Some(kind))
}

#[derive(Debug, Clone, Copy)]
enum LayoutKind {
    Request,
    Response,
    Callback,
}

impl From<FrameDirection> for LayoutKind {
    fn from(direction: FrameDirection) -> Self {
        match direction {
            FrameDirection::Request => Self::Request,
            FrameDirection::Response => Self::Response,
        }
    }
}

impl std::fmt::Display for LayoutKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request => formatter.write_str("request"),
            Self::Response => formatter.write_str("response"),
            Self::Callback => formatter.write_str("callback"),
        }
    }
}

fn serial_fallback_summary(command_name: &str, kind: Option<LayoutKind>) -> String {
    match kind {
        Some(LayoutKind::Request) => format!("{command_name} request"),
        Some(LayoutKind::Response) => format!("{command_name} response"),
        Some(LayoutKind::Callback) => format!("{command_name} callback"),
        None => command_name.to_string(),
    }
}

fn frame_type_name(definitions: &ZWaveDefinitions, frame_type: u8) -> String {
    definitions
        .frame_type(frame_type)
        .map(|definition| definition.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[derive(Debug, Default)]
struct LayoutResult {
    values: BTreeMap<String, FieldValue>,
    nested_summary: Option<String>,
    consumed: usize,
}

#[derive(Debug, Clone, Default)]
struct FieldValue {
    text: String,
    number: Option<u64>,
}

#[derive(Debug, Default)]
struct NestedCommand {
    summary: Option<String>,
}

fn collect_layout_fields<'a>(layout: &'a [LayoutField], map: &mut BTreeMap<&'a str, &'a FieldDef>) {
    for item in layout {
        match item {
            LayoutField::Field(field) => {
                map.insert(field.name.as_str(), field);
            }
            LayoutField::Group(group) => {
                for field in &group.fields {
                    map.insert(field.name.as_str(), field);
                }
            }
        }
    }
}

fn resolve_length(field: Option<&FieldDef>, raw: u64) -> usize {
    if let Some(field) = field
        && let Some(value) = field
            .values
            .iter()
            .find(|candidate| u64::from(candidate.value) == raw)
        && let Some(length) = value.length
    {
        return length;
    }
    raw as usize
}

fn signed_group_value(raw: u64, length: usize) -> i64 {
    match length {
        1 => i64::from(raw as i8),
        2 => i64::from(raw as i16),
        4 => i64::from(raw as i32),
        _ => raw as i64,
    }
}

fn decode_group_value(field: &FieldDef, raw: u64, length: usize) -> FieldValue {
    match field.kind {
        FieldType::Bool => FieldValue {
            text: if raw == 0 { "Off" } else { "On" }.to_string(),
            number: Some(raw),
        },
        FieldType::Enum => {
            let known = field
                .values
                .iter()
                .find(|candidate| u64::from(candidate.value) == raw)
                .map(|candidate| candidate.name.as_str());
            let text = match (known, field.raw_suffix) {
                (Some(name), true) => format!("{name} (0x{raw:0width$x})", width = length * 2),
                (Some(name), false) => name.to_string(),
                (None, _) => format!(
                    "{} (0x{raw:0width$x})",
                    field.unknown.as_deref().unwrap_or("Unknown"),
                    width = length * 2
                ),
            };
            FieldValue {
                text,
                number: Some(raw),
            }
        }
        FieldType::Flags => {
            let names = field
                .values
                .iter()
                .filter(|candidate| raw & u64::from(candidate.value) != 0)
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>();
            let text = if names.is_empty() {
                format!("0x{raw:0width$x}", width = length * 2)
            } else if field.raw_suffix {
                format!(
                    "{} (0x{raw:0width$x})",
                    names.join(", "),
                    width = length * 2
                )
            } else {
                names.join(", ")
            };
            FieldValue {
                text,
                number: Some(raw),
            }
        }
        FieldType::Int => {
            let value = signed_group_value(raw, length);
            FieldValue {
                text: value.to_string(),
                number: None,
            }
        }
        FieldType::Uint8
        | FieldType::Uint16
        | FieldType::Uint32
        | FieldType::Node
        | FieldType::Duration
        | FieldType::TxOptions
        | FieldType::CallbackId => numeric_value(field, Some(raw), length),
        _ => FieldValue {
            text: raw.to_string(),
            number: Some(raw),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_group(
    group: &PackedGroupDef,
    fields: &mut Vec<DecodedField>,
    data: &[u8],
    data_offset: usize,
    parsed: &mut LayoutResult,
    issues: &mut Vec<String>,
    context: &str,
) -> usize {
    let length = group.length;
    if length == 0 || length > 8 {
        issues.push(format!(
            "{context}: group '{}' has unsupported length {length}",
            group.group
        ));
        return 0;
    }
    if data.len() < length {
        issues.push(format!(
            "{context}: group '{}' needs {length} byte(s) at offset 0x{:04x}, found {}",
            group.group,
            data_offset,
            data.len()
        ));
        return data.len();
    }
    let mut raw = 0u64;
    for byte in &data[..length] {
        raw = (raw << 8) | u64::from(*byte);
    }
    for field in &group.fields {
        let Some(mask) = field.mask else {
            issues.push(format!(
                "{context}: packed field '{}' has no mask",
                field.name
            ));
            continue;
        };
        if mask == 0 || field.shift >= 64 {
            issues.push(format!(
                "{context}: packed field '{}' has an invalid mask/shift",
                field.name
            ));
            continue;
        }
        let value = (raw & mask) >> field.shift;
        let decoded = decode_group_value(field, value, length);
        fields.push(DecodedField {
            label: field.label.clone(),
            value: decoded.text.clone(),
            kind: field_kind(field),
            range: data_offset..data_offset + length,
        });
        if !field.name.is_empty() {
            parsed.values.insert(field.name.clone(), decoded);
        }
    }
    length
}

#[allow(clippy::too_many_arguments)]
fn decode_field_layout(
    definitions: &ZWaveDefinitions,
    fields: &mut Vec<DecodedField>,
    layout: &[LayoutField],
    data: &[u8],
    data_offset: usize,
    meta: &mut DecodeMeta,
    issues: &mut Vec<String>,
    context: &str,
) -> LayoutResult {
    let mut parsed = LayoutResult::default();
    let mut cursor = 0;
    let mut field_lookup = BTreeMap::new();
    collect_layout_fields(layout, &mut field_lookup);

    for item in layout {
        let field = match item {
            LayoutField::Field(field) => field,
            LayoutField::Group(group) => {
                let length = decode_group(
                    group,
                    fields,
                    &data[cursor..],
                    data_offset + cursor,
                    &mut parsed,
                    issues,
                    context,
                );
                cursor = cursor.saturating_add(length).min(data.len());
                continue;
            }
        };

        if field.remaining {
            let bytes = &data[cursor..];
            if field.required && bytes.is_empty() {
                issues.push(format!(
                    "{context}: missing {} at offset 0x{:04x}",
                    field.label,
                    data_offset + cursor
                ));
            }
            if !bytes.is_empty() {
                push_field(
                    fields,
                    field,
                    bytes,
                    data_offset + cursor,
                    &mut parsed,
                    meta,
                );
            }
            cursor = data.len();
            continue;
        }

        let mut length = match field.kind.fixed_width() {
            Some(width) => width,
            None => {
                if let Some(source) = &field.length_from {
                    match parsed.values.get(source).and_then(|value| value.number) {
                        Some(length) => {
                            resolve_length(field_lookup.get(source.as_str()).copied(), length)
                        }
                        None => {
                            issues.push(format!(
                                "{context}: {} has unknown length source '{source}'",
                                field.label
                            ));
                            break;
                        }
                    }
                } else if let Some(source) = &field.count_from {
                    match parsed.values.get(source).and_then(|value| value.number) {
                        Some(count) => count as usize,
                        None => {
                            issues.push(format!(
                                "{context}: {} has unknown count source '{source}'",
                                field.label
                            ));
                            break;
                        }
                    }
                } else if let Some(length) = field.length {
                    length
                } else {
                    // Unconstrained byte fields consume the rest of the layout.
                    data.len().saturating_sub(cursor)
                }
            }
        };

        let available = data.len().saturating_sub(cursor);
        let fixed_width = field.kind.fixed_width();
        if length > available {
            if available > 0 || field.required {
                issues.push(format!(
                    "{context}: {} needs {length} byte(s) at offset 0x{:04x}, found {available}",
                    field.label,
                    data_offset + cursor
                ));
            }
            if fixed_width.is_some() {
                // A partial fixed-width value cannot be decoded without guessing.
                break;
            }
            length = available;
        }

        if fixed_width.is_some() && length == 0 {
            if field.required {
                issues.push(format!(
                    "{context}: missing {} at offset 0x{:04x}",
                    field.label,
                    data_offset + cursor
                ));
            }
            break;
        }

        if fixed_width.is_none() && length == 0 {
            if field.required {
                issues.push(format!(
                    "{context}: missing {} at offset 0x{:04x}",
                    field.label,
                    data_offset + cursor
                ));
            }
            continue;
        }

        let bytes = &data[cursor..cursor + length];
        if field.kind == FieldType::CommandClassData {
            let nested = decode_command_class(
                definitions,
                fields,
                bytes,
                data_offset + cursor,
                meta,
                issues,
                context,
            );
            if nested.summary.is_some() {
                parsed.nested_summary = nested.summary;
            }
        } else {
            push_field(
                fields,
                field,
                bytes,
                data_offset + cursor,
                &mut parsed,
                meta,
            );
        }

        cursor += length;
    }

    parsed.consumed = cursor;
    parsed
}

fn push_field(
    fields: &mut Vec<DecodedField>,
    field: &FieldDef,
    bytes: &[u8],
    offset: usize,
    parsed: &mut LayoutResult,
    meta: &mut DecodeMeta,
) {
    let value = decode_field_value(field, bytes);
    fields.push(DecodedField {
        label: field.label.clone(),
        value: value.text.clone(),
        kind: field_kind(field),
        range: offset..offset + bytes.len(),
    });

    if !field.name.is_empty() {
        parsed.values.insert(field.name.clone(), value);
    }

    match field.role {
        FieldRole::Target => {
            if let Some(node) = bytes.first() {
                meta.targets.push(*node);
            }
        }
        FieldRole::Targets => {
            meta.targets.extend_from_slice(bytes);
        }
        FieldRole::Source => {
            meta.source = bytes.first().copied();
        }
        FieldRole::Value | FieldRole::Length | FieldRole::CommandClass => {}
    }
}

fn decode_field_value(field: &FieldDef, bytes: &[u8]) -> FieldValue {
    match field.kind {
        FieldType::Node => numeric_value(field, bytes.first().copied().map(u64::from), 1),
        FieldType::Uint8 => numeric_value(field, bytes.first().copied().map(u64::from), 1),
        FieldType::Uint16 => {
            let value =
                (bytes.len() >= 2).then(|| u64::from(u16::from_be_bytes([bytes[0], bytes[1]])));
            numeric_value(field, value, 2)
        }
        FieldType::Uint32 => {
            let value = (bytes.len() >= 4)
                .then(|| u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])));
            numeric_value(field, value, 4)
        }
        FieldType::Int => {
            let value = match bytes.len() {
                1 => Some(i64::from(bytes[0] as i8)),
                2 => Some(i64::from(i16::from_be_bytes([bytes[0], bytes[1]]))),
                4 => Some(i64::from(i32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ]))),
                _ => None,
            };
            value.map_or_else(
                || FieldValue {
                    text: spaced_hex(bytes),
                    number: None,
                },
                |value| FieldValue {
                    text: value.to_string(),
                    number: None,
                },
            )
        }
        FieldType::Uint => {
            let value = bytes
                .iter()
                .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
            numeric_value(field, Some(value), bytes.len().max(1))
        }
        FieldType::Bool => {
            let value = bytes.first().copied().unwrap_or_default();
            FieldValue {
                text: if value == 0 { "Off" } else { "On" }.to_string(),
                number: Some(u64::from(value)),
            }
        }
        FieldType::Enum => {
            let value = bytes.first().copied().unwrap_or_default();
            match field.format {
                FieldFormat::Hex => FieldValue {
                    text: format!("0x{value:02x}"),
                    number: Some(u64::from(value)),
                },
                FieldFormat::Decimal => FieldValue {
                    text: value.to_string(),
                    number: Some(u64::from(value)),
                },
                _ => {
                    let masked = u64::from(value) & field.mask.unwrap_or(u64::from(u8::MAX));
                    let known = field
                        .values
                        .iter()
                        .find(|candidate| u64::from(candidate.value) == masked)
                        .map(|candidate| candidate.name.as_str());
                    let text = match (known, field.raw_suffix) {
                        (Some(name), true) => format!("{name} (0x{value:02x})"),
                        (Some(name), false) => name.to_string(),
                        (None, _) => format!(
                            "{} (0x{value:02x})",
                            field.unknown.as_deref().unwrap_or("Unknown")
                        ),
                    };
                    FieldValue {
                        text,
                        number: Some(u64::from(value)),
                    }
                }
            }
        }
        FieldType::Flags => {
            let value = bytes.first().copied().unwrap_or_default();
            let names = field
                .values
                .iter()
                .filter(|candidate| value & candidate.value != 0)
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>();
            let text = if names.is_empty() {
                format!("0x{value:02x}")
            } else if field.raw_suffix {
                format!("{} (0x{value:02x})", names.join(", "))
            } else {
                names.join(", ")
            };
            FieldValue {
                text,
                number: Some(u64::from(value)),
            }
        }
        FieldType::Duration => numeric_value(field, bytes.first().copied().map(u64::from), 1),
        FieldType::TxOptions | FieldType::CallbackId => {
            numeric_value(field, bytes.first().copied().map(u64::from), 1)
        }
        FieldType::NodeList => FieldValue {
            text: bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            number: None,
        },
        FieldType::Text => FieldValue {
            text: String::from_utf8_lossy(bytes)
                .trim_end_matches('\0')
                .trim_end()
                .to_string(),
            number: None,
        },
        FieldType::Hex | FieldType::CommandClassData => FieldValue {
            text: spaced_hex(bytes),
            number: None,
        },
    }
}

fn numeric_value(field: &FieldDef, value: Option<u64>, width: usize) -> FieldValue {
    let Some(value) = value else {
        return FieldValue::default();
    };
    FieldValue {
        text: format_numeric(field, value, width),
        number: Some(value),
    }
}

fn format_numeric(field: &FieldDef, value: u64, width: usize) -> String {
    let format = match field.format {
        FieldFormat::Default => match field.kind {
            FieldType::Duration => FieldFormat::Duration,
            FieldType::TxOptions => FieldFormat::TxOptions,
            FieldType::CallbackId => FieldFormat::Hex,
            _ => FieldFormat::Decimal,
        },
        format => format,
    };
    match format {
        FieldFormat::Hex => format!("0x{value:0width$x}", width = width * 2),
        FieldFormat::TxOptions => {
            let names = field
                .values
                .iter()
                .filter(|candidate| value & u64::from(candidate.value) != 0)
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>();
            let raw = format!("0x{value:0width$x}", width = width * 2);
            if names.is_empty() {
                raw
            } else {
                format!("{} ({raw})", names.join(", "))
            }
        }
        FieldFormat::Level => level_value(value as u8),
        FieldFormat::Duration => dimming_duration(value as u8),
        FieldFormat::MeterRateScale => meter_rate_scale(value as u8),
        FieldFormat::Decimal | FieldFormat::Default => value.to_string(),
    }
}

fn field_kind(field: &FieldDef) -> FieldKind {
    match field.role {
        FieldRole::Length => FieldKind::Length,
        FieldRole::Target | FieldRole::Targets | FieldRole::Source => FieldKind::Node,
        FieldRole::CommandClass => FieldKind::CommandClass,
        FieldRole::Value => match field.kind {
            FieldType::Node | FieldType::NodeList => FieldKind::Node,
            FieldType::CommandClassData => FieldKind::CommandClass,
            _ => FieldKind::Value,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_command_class(
    definitions: &ZWaveDefinitions,
    fields: &mut Vec<DecodedField>,
    data: &[u8],
    data_offset: usize,
    meta: &mut DecodeMeta,
    issues: &mut Vec<String>,
    context: &str,
) -> NestedCommand {
    let Some(class) = data.first().copied() else {
        return NestedCommand::default();
    };
    let class_definition = definitions.command_class(class);
    let class_name = class_definition
        .map(|definition| definition.name.clone())
        .unwrap_or_else(|| format!("Unknown 0x{class:02x}"));
    meta.command_class = Some(class);
    fields.push(DecodedField {
        label: "Command class".to_string(),
        value: format!("{class_name} (0x{class:02x})"),
        kind: FieldKind::CommandClass,
        range: data_offset..data_offset + 1,
    });

    let Some(command) = data.get(1).copied() else {
        return NestedCommand {
            summary: Some(class_name),
        };
    };
    let command_definition = class_definition.and_then(|definition| {
        definition
            .commands
            .iter()
            .find(|candidate| candidate.id == command)
    });
    let command_name = command_definition
        .map(|definition| definition.name.clone())
        .unwrap_or_else(|| format!("0x{command:02x}"));
    meta.command = Some(command);
    fields.push(DecodedField {
        label: "CC command".to_string(),
        value: format!("{command_name} (0x{command:02x})"),
        kind: FieldKind::CommandClass,
        range: data_offset + 1..data_offset + 2,
    });

    let values = &data[2..];
    let Some(command_definition) = command_definition else {
        add_raw_data(fields, "CC data", values, data_offset + 2);
        return NestedCommand {
            summary: Some(format!("{class_name} {command_name}")),
        };
    };

    let command_context = format!("{context}: {class_name} {command_name}");
    let parsed = decode_field_layout(
        definitions,
        fields,
        &command_definition.fields,
        values,
        data_offset + 2,
        meta,
        issues,
        &command_context,
    );
    let leftover = values.len().saturating_sub(parsed.consumed);
    if leftover > 0 {
        add_raw_data(
            fields,
            "CC data",
            &values[parsed.consumed..],
            data_offset + 2 + parsed.consumed,
        );
    }

    let detail = command_definition
        .summary
        .as_deref()
        .map(|template| render_template(template, &parsed.values, None))
        .unwrap_or_default();
    let summary = if detail.is_empty() {
        format!("{class_name} {command_name}")
    } else {
        format!("{class_name} {command_name} · {detail}")
    };
    NestedCommand {
        summary: Some(summary),
    }
}

fn add_raw_data(fields: &mut Vec<DecodedField>, label: &str, data: &[u8], offset: usize) {
    if data.is_empty() {
        return;
    }
    fields.push(DecodedField {
        label: label.to_string(),
        value: spaced_hex(data),
        kind: FieldKind::Value,
        range: offset..offset + data.len(),
    });
}

fn render_template(
    template: &str,
    values: &BTreeMap<String, FieldValue>,
    nested_summary: Option<&str>,
) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let Some(end) = rest[start + 1..].find('}') else {
            result.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = &rest[start + 1..start + 1 + end];
        let (name, modifier) = token
            .split_once(':')
            .map_or((token, None), |(name, modifier)| (name, Some(modifier)));
        let value = if name == "commandClassSummary" {
            nested_summary.unwrap_or_default().to_string()
        } else if let Some(field) = values.get(name) {
            match modifier {
                Some("number") => field
                    .number
                    .map(|number| number.to_string())
                    .unwrap_or_else(|| field.text.clone()),
                Some("hex") => field
                    .number
                    .map(|number| format!("0x{number:02x}"))
                    .unwrap_or_else(|| field.text.clone()),
                _ => field.text.clone(),
            }
        } else {
            String::new()
        };
        result.push_str(&value);
        rest = &rest[start + 1 + end + 1..];
    }
    result.push_str(rest);
    trim_summary(result)
}

fn trim_summary(mut text: String) -> String {
    loop {
        text = text.trim().to_string();
        let trimmed = text
            .trim_end_matches(|character: char| {
                character.is_whitespace() || matches!(character, '·' | ',' | ':')
            })
            .to_string();
        if trimmed == text {
            return text;
        }
        text = trimmed;
    }
}

fn level_value(value: u8) -> String {
    match value {
        0x00 => "Off (0%)".to_string(),
        0xff => "On (100%)".to_string(),
        value if value <= 99 => format!("{value}%"),
        value => format!("0x{value:02x}"),
    }
}

fn dimming_duration(value: u8) -> String {
    match value {
        0x00 => "instant".to_string(),
        0x01..=0x7f => format!("{value} s"),
        0x80..=0xfe => format!("{} min", value - 0x7f),
        _ => "factory default".to_string(),
    }
}

fn meter_rate_scale(value: u8) -> String {
    format!("rate {}, scale {}", value & 0x03, (value >> 2) & 0x07)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::compact_hex;
    use crate::parsed::FrameKind;

    #[test]
    fn version_frame_round_trips() {
        let frame = response(&[
            SERIAL_GET_VERSION,
            b'Z',
            b'-',
            b'W',
            b'a',
            b'v',
            b'e',
            b' ',
            b'4',
            b'.',
            b'0',
            b'5',
            0,
            1,
        ]);
        assert_eq!(compact_hex(&frame), "011001155a2d5761766520342e3035000197");
        let decoded = decode(&frame);
        assert_eq!(decoded.frames.len(), 1);
        assert!(decoded.issues.is_empty());
        assert_eq!(decoded.frames[0].checksum_ok, Some(true));
        assert_eq!(decoded.frames[0].title, "ZW_Version · Response");
        assert_eq!(decoded.summary, "ZW_Version · Z-Wave 4.05");
        let fields = &decoded.frames[0].fields;
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Library version" && field.value == "Z-Wave 4.05")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Library type" && field.value == "Static controller")
        );
        assert!(!fields.iter().any(|field| field.label == "Version data"));
    }

    #[test]
    fn decodes_controller_capabilities() {
        let decoded = decode(&[0x01, 0x04, 0x01, 0x05, 0x3c, 0xc3]);
        assert!(decoded.issues.is_empty());
        assert_eq!(
            decoded.summary,
            "Controller capabilities · SIS present, was real primary, SUC, no nodes included (0x3c)"
        );
    }

    #[test]
    fn decodes_serial_api_capabilities() {
        let frame = [
            0x01, 0x2b, 0x01, 0x07, 0x03, 0x07, 0x00, 0x86, 0x00, 0x02, 0x00, 0x01, 0xfe, 0x80,
            0xfe, 0x88, 0x0f, 0x00, 0x00, 0x00, 0xfb, 0x97, 0x7f, 0x82, 0x07, 0x00, 0x00, 0x80,
            0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xc2,
        ];
        let decoded = decode(&frame);
        assert!(decoded.issues.is_empty());
        assert_eq!(decoded.summary, "Serial API 3.7 · 0x0086");
        let fields = &decoded.frames[0].fields;
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Manufacturer ID" && field.value == "0x0086")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Product type" && field.value == "0x0002")
        );
    }

    #[test]
    fn decodes_thermostat_mode_report() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            4,
            3,
            0x40,
            0x03,
            0x01,
        ]);
        let decoded = decode(&frame);
        assert!(decoded.issues.is_empty());
        assert_eq!(decoded.frames.len(), 1);
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Mode" && field.value == "Heat")
        );
    }

    #[test]
    fn decodes_thermostat_setpoint_report() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            9,
            5,
            0x43,
            0x03,
            0x01,
            0x00,
            0x15,
        ]);
        let decoded = decode(&frame);
        assert!(decoded.issues.is_empty());
        let fields = &decoded.frames[0].fields;
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Setpoint type" && field.value == "Heating")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Size" && field.value == "1 byte (8 bits)")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Scale" && field.value == "Celsius")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Precision" && field.value == "0")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Value" && field.value == "21")
        );
    }

    #[test]
    fn request_version_frame() {
        let frame = request(&[SERIAL_GET_VERSION]);
        assert_eq!(compact_hex(&frame), "01030015e9");
        let decoded = decode(&frame);
        assert_eq!(decoded.frames[0].summary, "ZW_Version request");
        assert!(decoded.issues.is_empty());
    }

    #[test]
    fn decodes_application_command_basic_set() {
        let frame = request(&[SERIAL_APPLICATION_COMMAND_HANDLER, 0, 4, 3, CC_BASIC, 1, 45]);
        let decoded = decode(&frame);
        assert_eq!(decoded.frames.len(), 1);
        assert!(decoded.summary.contains("Basic Set"));
        assert!(decoded.summary.contains("45%"));
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Source node" && field.value == "4")
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Command class" && field.value == "Basic (0x20)")
        );
    }

    #[test]
    fn decodes_configuration_values_as_signed_integers() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            8,
            7,
            0x70,
            0x06,
            0x00,
            0x01,
            0x02,
            0xFF,
            0xFB,
        ]);
        let decoded = decode(&frame);
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Size" && field.value == "2 bytes (16 bits)")
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Value" && field.value == "-5")
        );

        let properties = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            8,
            11,
            0x70,
            0x0F,
            0x00,
            0x01,
            0x01,
            0x00,
            0x64,
            0x00,
            0x00,
            0x02,
            0x00,
        ]);
        let decoded = decode(&properties);
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Format" && field.value == "Signed integer")
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Maximum value" && field.value == "100")
        );
    }

    #[test]
    fn decodes_application_controller_update_node_info() {
        let frame = request(&[0x49, 0x84, 4, 3, 0x04, 0x11, 0x06]);
        let decoded = decode(&frame);
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Update status"
                    && field.value == "Node info received (0x84)")
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Node" && field.value == "4")
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Node information" && field.value == "04 11 06")
        );
    }

    #[test]
    fn decodes_multilevel_and_meter() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            7,
            4,
            CC_SWITCH_MULTILEVEL,
            3,
            72,
            5,
        ]);
        let decoded = decode(&frame);
        assert_eq!(decoded.summary, "SwitchMultilevel Report · 72% · 5 s");

        let sensor = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            6,
            6,
            0x31,
            0x05,
            0x01,
            0x42,
            0x07,
            0xD1,
        ]);
        let decoded = decode(&sensor);
        assert!(decoded.summary.contains("Multilevel Sensor"));
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| { field.label == "Size" && field.value == "2 bytes (16 bits)" })
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| { field.label == "Precision" && field.value == "2" })
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| { field.label == "Value" && field.value == "2001" })
        );

        let meter = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            9,
            8,
            CC_METER,
            2,
            0x01,
            0x04,
            0,
            0,
            0x30,
            0x39,
        ]);
        let decoded = decode(&meter);
        assert!(decoded.summary.contains("12345"));
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| { field.label == "Rate type" && field.value == "Default (0x00)" })
        );
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| { field.label == "Size" && field.value == "4 bytes (32 bits)" })
        );
    }

    #[test]
    fn flags_are_rendered_from_definitions() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0x03,
            4,
            2,
            CC_BASIC,
            0x02,
        ]);
        let decoded = decode(&frame);
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "RX status"
                    && field.value == "low power, no route (0x03)")
        );
    }

    #[test]
    fn optional_trailing_fields_do_not_leave_separators() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            7,
            3,
            CC_SWITCH_MULTILEVEL,
            3,
            72,
        ]);
        let decoded = decode(&frame);
        assert_eq!(decoded.summary, "SwitchMultilevel Report · 72%");
        assert!(decoded.issues.is_empty());
    }

    #[test]
    fn decodes_send_data_callback_and_nested_command() {
        let outgoing = request(&[
            SERIAL_SEND_DATA,
            5,
            3,
            CC_SWITCH_BINARY,
            0x01,
            0xff,
            0x25,
            0x01,
        ]);
        let decoded = decode(&outgoing);
        assert_eq!(decoded.summary, "ZW_SendData · node 5 · 3 byte(s)");
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "CC command" && field.value == "Set (0x01)")
        );
        assert!(decoded.frames[0].fields.iter().any(|field| {
            field.label == "TX options" && field.value == "ACK, AUTO_ROUTE, EXPLORE (0x25)"
        }));

        let callback = request(&[SERIAL_SEND_DATA, 0x21, 0x00]);
        let decoded = decode(&callback);
        assert_eq!(decoded.summary, "ZW_SendData callback · Complete OK (0x00)");
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Callback ID" && field.value == "0x21")
        );
    }

    #[test]
    fn extracts_routing_and_command_metadata() {
        let send = request(&[
            SERIAL_SEND_DATA,
            5,
            3,
            CC_SWITCH_BINARY,
            0x01,
            0xff,
            0x01,
            0x01,
        ]);
        let address = &decode(&send).frames[0].address;
        assert_eq!(address.targets, vec![5]);
        assert_eq!(address.source, None);
        assert_eq!(address.command_class, Some(CC_SWITCH_BINARY));
        assert_eq!(address.command, Some(0x01));

        let multi = request(&[
            SERIAL_SEND_DATA_MULTI,
            2,
            5,
            7,
            3,
            CC_SWITCH_MULTILEVEL,
            0x03,
            72,
            0x01,
            0x01,
        ]);
        let decoded = decode(&multi);
        let address = &decoded.frames[0].address;
        assert_eq!(address.targets, vec![5, 7]);
        assert_eq!(address.command_class, Some(CC_SWITCH_MULTILEVEL));
        assert_eq!(address.command, Some(0x03));
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "Nodes" && field.value == "5, 7")
        );

        let incoming = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            9,
            3,
            CC_BASIC,
            0x03,
            0xff,
        ]);
        let address = &decode(&incoming).frames[0].address;
        assert_eq!(address.targets, Vec::<u8>::new());
        assert_eq!(address.source, Some(9));
        assert_eq!(address.command_class, Some(CC_BASIC));
        assert_eq!(address.command, Some(0x03));

        assert!(
            decode(&request(&[SERIAL_GET_VERSION])).frames[0]
                .address
                .is_empty()
        );
    }

    #[test]
    fn unknown_command_keeps_raw_bytes() {
        let frame = request(&[
            SERIAL_APPLICATION_COMMAND_HANDLER,
            0,
            4,
            4,
            CC_BASIC,
            0x7f,
            0xaa,
            0xbb,
        ]);
        let decoded = decode(&frame);
        assert!(decoded.summary.contains("Basic 0x7f"));
        assert!(
            decoded.frames[0]
                .fields
                .iter()
                .any(|field| field.label == "CC data" && field.value == "aa bb")
        );
    }

    #[test]
    fn truncated_fields_are_reported() {
        let frame = request(&[SERIAL_APPLICATION_COMMAND_HANDLER, 0, 4, 2, CC_BASIC, 1]);
        let decoded = decode(&frame);
        assert!(
            decoded
                .issues
                .iter()
                .any(|issue| issue.contains("Value needs 1 byte"))
        );
        assert!(decoded.summary.contains("Basic Set"));
    }

    #[test]
    fn reports_bad_checksums_without_failing() {
        let mut frame = request(&[SERIAL_GET_VERSION]);
        *frame.last_mut().unwrap() ^= 0xff;
        let decoded = decode(&frame);
        assert_eq!(decoded.frames[0].checksum_ok, Some(false));
        assert!(decoded.issues[0].contains("invalid checksum"));
    }

    #[test]
    fn handles_controls_and_unknown_bytes() {
        let mut payload = vec![FRAME_ACK, 0xaa, 0xbb];
        payload.extend_from_slice(&request(&[SERIAL_GET_VERSION]));
        payload.push(FRAME_NAK);
        let decoded = decode(&payload);
        assert_eq!(decoded.frames[0].kind, FrameKind::Ack);
        assert_eq!(decoded.frames[1].kind, FrameKind::Unknown);
        assert_eq!(decoded.frames[2].kind, FrameKind::Data);
        assert_eq!(decoded.frames[3].kind, FrameKind::Nak);
        assert!(!decoded.issues.is_empty());
    }

    #[test]
    fn reports_incomplete_frames() {
        let frame = request(&[SERIAL_GET_VERSION]);
        let decoded = decode(&frame[..3]);
        assert_eq!(decoded.frames[0].kind, FrameKind::Unknown);
        assert!(decoded.issues[0].contains("incomplete frame"));
    }

    #[test]
    fn empty_payload_is_not_an_error() {
        let decoded = decode(&[]);
        assert!(decoded.frames.is_empty());
        assert!(decoded.issues.is_empty());
        assert_eq!(decoded.summary, "Empty payload");
    }

    #[test]
    fn definitions_supply_command_names() {
        assert_eq!(
            serial_command_name(SERIAL_GET_VERSION).as_deref(),
            Some("ZW_Version")
        );
        assert_eq!(command_class_name(CC_METER).as_deref(), Some("Meter"));
        assert_eq!(serial_command_name(0xfe), None);
    }
}
