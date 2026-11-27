//! ZigBee EZSP and ASH decoding.
//!
//! The gateway strips ASH framing from EZSP messages, but it records ASH link
//! frames (ACK/NAK/RST/RSTACK/ERROR) as raw wire bytes. The decoder therefore
//! first tries a strict ASH link-frame parse, then falls back to the canonical
//! EZSP frame layout used by the gateway:
//!
//! ```text
//! seq | frameControlLow | frameControlHigh = 0x01 | frameId (2, LE) | params
//! ```
//!
//! ZCL payloads carried by the messaging commands are decoded on top of the
//! EZSP layout. Unknown commands and clusters fall back to raw bytes.

use crate::definitions::{
    Endian, FieldDef, FieldFormat, FieldRole, FieldType, FilterSlot, LayoutField,
};
use crate::hex::spaced_hex;
use crate::parsed::{DecodeResult, DecodedField, DecodedFrame, FieldKind, FrameAddress, FrameKind};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock, RwLock};

pub const ASH_FLAG: u8 = 0x7e;
pub const ASH_ESCAPE: u8 = 0x7d;
pub const ASH_XON: u8 = 0x11;
pub const ASH_XOFF: u8 = 0x13;
pub const ASH_SUBSTITUTE: u8 = 0x18;
pub const ASH_CANCEL: u8 = 0x1a;

pub const ASH_CONTROL_RESPONSE: u8 = 0x80;
pub const ASH_CONTROL_ASYNC_CALLBACK: u8 = 0x10;
pub const ASH_CONTROL_SYNC_CALLBACK: u8 = 0x08;

pub const EZSP_FRAME_FORMAT_1: u8 = 0x01;
pub const EZSP_HEADER_LEN: usize = 5;

pub const EZSP_VERSION: u16 = 0x0000;
pub const EZSP_NETWORK_INIT: u16 = 0x0017;
pub const EZSP_NETWORK_STATE: u16 = 0x0018;
pub const EZSP_FORM_NETWORK: u16 = 0x001e;
pub const EZSP_LEAVE_NETWORK: u16 = 0x0020;
pub const EZSP_PERMIT_JOINING: u16 = 0x0022;
pub const EZSP_GET_EUI64: u16 = 0x0026;
pub const EZSP_GET_NODE_ID: u16 = 0x0027;
pub const EZSP_GET_NETWORK_PARAMETERS: u16 = 0x0028;
pub const EZSP_SEND_UNICAST: u16 = 0x0034;
pub const EZSP_SEND_BROADCAST: u16 = 0x0036;
pub const EZSP_SEND_MULTICAST: u16 = 0x0038;
pub const EZSP_MESSAGE_SENT_HANDLER: u16 = 0x003f;
pub const EZSP_INCOMING_MESSAGE_HANDLER: u16 = 0x0045;
pub const EZSP_GET_CHILD_DATA: u16 = 0x004a;
pub const EZSP_GET_CONFIGURATION_VALUE: u16 = 0x0052;
pub const EZSP_SET_CONFIGURATION_VALUE: u16 = 0x0053;
pub const EZSP_GET_CURRENT_SECURITY_STATE: u16 = 0x0069;
pub const EZSP_GET_VALUE: u16 = 0x00aa;
pub const EZSP_SET_VALUE: u16 = 0x00ab;
pub const EZSP_CLEAR_KEY_TABLE: u16 = 0x00b1;

/// CRC-16/CCITT (XMODEM): polynomial 0x1021, initial value 0xffff.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
fn stuff(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    for byte in bytes {
        if matches!(
            *byte,
            ASH_FLAG | ASH_ESCAPE | ASH_XON | ASH_XOFF | ASH_SUBSTITUTE | ASH_CANCEL
        ) {
            out.push(ASH_ESCAPE);
            out.push(*byte ^ 0x20);
        } else {
            out.push(*byte);
        }
    }
    out
}

fn unstuff(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut escaped = false;
    for byte in bytes {
        if escaped {
            let value = *byte ^ 0x20;
            if !matches!(
                value,
                ASH_FLAG | ASH_ESCAPE | ASH_XON | ASH_XOFF | ASH_SUBSTITUTE | ASH_CANCEL
            ) {
                return None;
            }
            out.push(value);
            escaped = false;
        } else if *byte == ASH_ESCAPE {
            escaped = true;
        } else {
            out.push(*byte);
        }
    }
    if escaped { None } else { Some(out) }
}

/// Builds a control-only ASH frame (ACK, NAK, RST, RSTACK, ERROR).
#[cfg(test)]
fn encode_control_frame(control: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 3);
    body.push(control);
    body.extend_from_slice(data);
    let crc = crc16(&body);
    body.extend_from_slice(&crc.to_be_bytes());
    let mut out = stuff(&body);
    out.push(ASH_FLAG);
    out
}

/// Builds a canonical EZSP request frame.
pub fn build_request(seq: u8, command: u16, params: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(EZSP_HEADER_LEN + params.len());
    frame.push(seq);
    frame.push(0x00);
    frame.push(EZSP_FRAME_FORMAT_1);
    frame.extend_from_slice(&command.to_le_bytes());
    frame.extend_from_slice(params);
    frame
}

pub fn command_name(id: u16) -> Option<String> {
    current().command(id).map(|command| command.name.clone())
}

/// Name of a general ZCL command, for example `Report attributes`.
pub fn zcl_general_command_name(id: u8) -> Option<String> {
    current()
        .general_command(id)
        .map(|command| command.name.clone())
}

/// Byte offset of a named field in a canonical EZSP frame, when the command
/// has a fixed-width layout before it. Both request and response layouts are
/// considered. Returns `None` for unknown commands or variable-width prefixes.
pub fn field_offset(command_id: u16, field_name: &str) -> Option<usize> {
    current().field_offset(command_id, field_name)
}

/// Byte offset of the ZCL payload field in a canonical EZSP frame.
pub fn message_contents_offset(command_id: u16) -> Option<usize> {
    current().field_offset_by_role(command_id, FieldRole::Payload)
}

/// Raw parameter bytes for each field in the command's request layout (or
/// response layout for incoming-only callbacks). Used by the builder to
/// restore structured fields from a decoded frame.
pub fn parameter_field_bytes(command_id: u16, params: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let definitions = current();
    let command = definitions.command(command_id)?;
    let layout = if command.incoming_only {
        &command.response_fields
    } else {
        &command.fields
    };
    let decoded = decode_layout(params, layout, EZSP_HEADER_LEN);
    let mut fields = BTreeMap::new();
    for (name, range) in decoded.ranges {
        let Some(start) = range.start.checked_sub(EZSP_HEADER_LEN) else {
            continue;
        };
        let Some(end) = range.end.checked_sub(EZSP_HEADER_LEN) else {
            continue;
        };
        if start <= end && end <= params.len() {
            fields.insert(name, params[start..end].to_vec());
        }
    }
    Some(fields)
}

fn layout_field_layout(
    layout: &[LayoutField],
    matches: impl Fn(&FieldDef) -> bool,
) -> Option<(usize, &FieldDef)> {
    let mut cursor = 0;
    for item in layout {
        if let LayoutField::Group(group) = item {
            cursor += group.length;
            continue;
        }
        let LayoutField::Field(field) = item else {
            return None;
        };
        if matches(field) {
            return Some((cursor, field));
        }
        if field.derived {
            continue;
        }
        if field.length_from.is_some() || field.count_from.is_some() || field.remaining {
            return None;
        }
        cursor += field.length.or_else(|| field.kind.fixed_width())?;
    }
    None
}

fn layout_field_offset(
    layout: &[LayoutField],
    matches: impl Fn(&FieldDef) -> bool,
) -> Option<usize> {
    layout_field_layout(layout, matches).map(|(offset, _)| offset)
}

fn layout_field_by_role(layout: &[LayoutField], role: FieldRole) -> Option<&FieldDef> {
    layout
        .iter()
        .filter_map(LayoutField::field)
        .find(|field| field.role == role)
}

fn layout_has_role(layout: &[LayoutField], role: FieldRole) -> bool {
    layout
        .iter()
        .filter_map(LayoutField::field)
        .any(|field| field.role == role)
}

/// Decodes either a raw ASH link frame or a canonical EZSP frame.
pub fn decode(payload: &[u8]) -> DecodeResult {
    if let Some(frame) = decode_link_frame(payload) {
        return DecodeResult {
            summary: frame.summary.clone(),
            frames: vec![frame],
            issues: Vec::new(),
        };
    }
    decode_ezsp(payload)
}

/// True when the payload is a complete, valid ASH link frame. This deliberately
/// excludes DATA frames: those are represented as EZSP messages.
pub fn is_link_frame(payload: &[u8]) -> bool {
    decode_link_frame(payload).is_some()
}

fn decode_link_frame(payload: &[u8]) -> Option<DecodedFrame> {
    if payload.len() < 4 || payload.last() != Some(&ASH_FLAG) {
        return None;
    }
    let frame = unstuff(&payload[..payload.len() - 1])?;
    if frame.len() < 3 {
        return None;
    }
    let crc_index = frame.len() - 2;
    let expected = crc16(&frame[..crc_index]);
    let actual = u16::from_be_bytes([frame[crc_index], frame[crc_index + 1]]);
    if expected != actual {
        return None;
    }

    let control = frame[0];
    let data = &frame[1..crc_index];
    let (kind, title, summary, mut fields) = match control & 0xe0 {
        0x80 => {
            let ack = control & 0x07;
            let nready = control & 0x08 != 0;
            (
                FrameKind::Ack,
                "ASH ACK".to_string(),
                format!("ASH ACK · ack {ack}"),
                vec![
                    control_field(control),
                    DecodedField {
                        label: "Frame".to_string(),
                        value: "ACK".to_string(),
                        kind: FieldKind::FrameType,
                        range: 0..1,
                    },
                    DecodedField {
                        label: "ACK number".to_string(),
                        value: ack.to_string(),
                        kind: FieldKind::Value,
                        range: 0..1,
                    },
                    DecodedField {
                        label: "nReady".to_string(),
                        value: nready.to_string(),
                        kind: FieldKind::Value,
                        range: 0..1,
                    },
                ],
            )
        }
        0xa0 => {
            let ack = control & 0x07;
            let nready = control & 0x08 != 0;
            (
                FrameKind::Nak,
                "ASH NAK".to_string(),
                format!("ASH NAK · ack {ack}"),
                vec![
                    control_field(control),
                    DecodedField {
                        label: "Frame".to_string(),
                        value: "NAK".to_string(),
                        kind: FieldKind::FrameType,
                        range: 0..1,
                    },
                    DecodedField {
                        label: "ACK number".to_string(),
                        value: ack.to_string(),
                        kind: FieldKind::Value,
                        range: 0..1,
                    },
                    DecodedField {
                        label: "nReady".to_string(),
                        value: nready.to_string(),
                        kind: FieldKind::Value,
                        range: 0..1,
                    },
                ],
            )
        }
        0xc0 => match control {
            0xc0 => (
                FrameKind::Reset,
                "ASH RST".to_string(),
                "ASH RST".to_string(),
                vec![control_field(control)],
            ),
            0xc1 | 0xc2 => {
                if data.len() != 2 {
                    return None;
                }
                let version = data[0];
                let code = data[1];
                let (kind, title) = if control == 0xc1 {
                    (FrameKind::ResetAck, "ASH RSTACK")
                } else {
                    (FrameKind::Error, "ASH ERROR")
                };
                (
                    kind,
                    title.to_string(),
                    format!("{title} · reset 0x{code:02x}"),
                    vec![
                        control_field(control),
                        DecodedField {
                            label: "Frame".to_string(),
                            value: title.to_string(),
                            kind: FieldKind::FrameType,
                            range: 0..1,
                        },
                        DecodedField {
                            label: "Version".to_string(),
                            value: version.to_string(),
                            kind: FieldKind::Value,
                            range: 1..2,
                        },
                        DecodedField {
                            label: "Reset code".to_string(),
                            value: format!("0x{code:02x}"),
                            kind: FieldKind::Value,
                            range: 2..3,
                        },
                    ],
                )
            }
            _ => return None,
        },
        _ => return None,
    };

    fields.push(DecodedField {
        label: "CRC".to_string(),
        value: format!("0x{actual:04x}"),
        kind: FieldKind::Checksum,
        range: crc_index..crc_index + 2,
    });

    Some(DecodedFrame {
        kind,
        range: 0..payload.len(),
        title,
        summary,
        checksum_ok: Some(true),
        fields,
        address: FrameAddress::default(),
    })
}

fn control_field(control: u8) -> DecodedField {
    DecodedField {
        label: "Control".to_string(),
        value: format!("0x{control:02x}"),
        kind: FieldKind::Control,
        range: 0..1,
    }
}

fn decode_ezsp(payload: &[u8]) -> DecodeResult {
    let mut result = DecodeResult::default();
    if payload.len() < EZSP_HEADER_LEN {
        result.issues.push(format!(
            "incomplete EZSP frame: expected at least {EZSP_HEADER_LEN} bytes, found {}",
            payload.len()
        ));
        result.summary = format!("Raw payload · {} bytes", payload.len());
        result.frames.push(raw_frame(payload));
        return result;
    }

    let definitions = current();
    let seq = payload[0];
    let control = payload[1];
    let high = payload[2];
    let command_id = u16::from_le_bytes([payload[3], payload[4]]);
    let params = &payload[5..];
    let response = control & ASH_CONTROL_RESPONSE != 0;
    let callback = control & (ASH_CONTROL_ASYNC_CALLBACK | ASH_CONTROL_SYNC_CALLBACK) != 0;
    let direction = if response {
        "Response"
    } else if callback {
        "Callback"
    } else {
        "Command"
    };

    if high & 0x03 != EZSP_FRAME_FORMAT_1 {
        result.issues.push(format!(
            "unexpected EZSP frame format version {}",
            high & 0x03
        ));
    }

    let definition = definitions.command(command_id);
    let layout = definition
        .map(|definition| {
            if response || callback || definition.incoming_only {
                definition.response_fields.as_slice()
            } else {
                definition.fields.as_slice()
            }
        })
        .unwrap_or(&[]);
    let decoded = decode_layout(params, layout, EZSP_HEADER_LEN);

    let mut fields = vec![
        DecodedField {
            label: "Sequence".to_string(),
            value: seq.to_string(),
            kind: FieldKind::Header,
            range: 0..1,
        },
        DecodedField {
            label: "Frame control".to_string(),
            value: format!("0x{control:02x} · {direction}"),
            kind: FieldKind::Header,
            range: 1..2,
        },
        DecodedField {
            label: "Frame ID".to_string(),
            value: definition
                .map(|definition| format!("{} (0x{command_id:04x})", definition.name))
                .unwrap_or_else(|| format!("0x{command_id:04x}")),
            kind: FieldKind::Command,
            range: 3..5,
        },
    ];
    fields.extend(decoded.fields);
    if definition.is_none() && !params.is_empty() {
        fields.push(DecodedField {
            label: "Parameters".to_string(),
            value: spaced_hex(params),
            kind: FieldKind::Value,
            range: EZSP_HEADER_LEN..payload.len(),
        });
    }

    let mut nested_summary = None;
    if let Some(payload_field) = layout_field_by_role(layout, FieldRole::Payload)
        && let Some(range) = decoded.ranges.get(&payload_field.name).cloned()
        && range.start >= EZSP_HEADER_LEN
        && range.end <= payload.len()
    {
        let cluster = layout_field_by_role(layout, FieldRole::Cluster)
            .and_then(|field| decoded.values.get(&field.name))
            .and_then(|value| value.number)
            .map(|value| value as u16);
        let zcl = decode_zcl(&payload[range.clone()], range.start, cluster, &definitions);
        fields.extend(zcl.fields);
        if !zcl.summary.is_empty() {
            nested_summary = Some(zcl.summary);
        }
        result.issues.extend(zcl.issues);
    }

    result.issues.extend(decoded.issues);

    let name = definition
        .map(|definition| definition.name.as_str())
        .unwrap_or("EZSP");
    let template = definition.and_then(|definition| {
        if response || callback {
            definition.response_summary.as_deref()
        } else {
            definition.request_summary.as_deref()
        }
    });
    let mut summary = template
        .map(|template| render_summary(template, &decoded.values))
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| format!("{name} · {direction}"));
    if let Some(nested) = nested_summary {
        summary.push_str(" · ");
        summary.push_str(&nested);
    }

    result.summary = summary.clone();
    result.frames.push(DecodedFrame {
        kind: FrameKind::Data,
        range: 0..payload.len(),
        title: format!("{name} · {direction}"),
        summary,
        checksum_ok: None,
        fields,
        address: FrameAddress::default(),
    });
    result
}

fn raw_frame(payload: &[u8]) -> DecodedFrame {
    DecodedFrame {
        kind: FrameKind::Unknown,
        range: 0..payload.len(),
        title: "Raw payload".to_string(),
        summary: format!("Raw payload · {} bytes", payload.len()),
        checksum_ok: None,
        fields: vec![DecodedField {
            label: "Payload".to_string(),
            value: spaced_hex(payload),
            kind: FieldKind::Value,
            range: 0..payload.len(),
        }],
        address: FrameAddress::default(),
    }
}

fn render_summary(template: &str, values: &BTreeMap<String, FieldValue>) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out.trim().to_string();
        };
        let key = &after[..end];
        if let Some(value) = values.get(key) {
            out.push_str(&value.text);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[derive(Debug, Clone, Default)]
struct LayoutResult {
    fields: Vec<DecodedField>,
    values: BTreeMap<String, FieldValue>,
    ranges: BTreeMap<String, Range<usize>>,
    issues: Vec<String>,
    consumed: usize,
}

fn decode_layout(data: &[u8], layout: &[LayoutField], base: usize) -> LayoutResult {
    let mut result = LayoutResult::default();
    let mut cursor = 0;

    for item in layout {
        if let LayoutField::Group(group) = item {
            let width = group.length;
            if cursor + width > data.len() {
                result.issues.push(format!(
                    "incomplete packed group '{}' at offset 0x{:04x}",
                    group.group,
                    base + cursor
                ));
                break;
            }
            let bytes = &data[cursor..cursor + width];
            result.fields.push(DecodedField {
                label: group.group.clone(),
                value: spaced_hex(bytes),
                kind: FieldKind::Value,
                range: base + cursor..base + cursor + width,
            });
            cursor += width;
            continue;
        }
        let LayoutField::Field(field) = item else {
            unreachable!()
        };
        if field.derived {
            continue;
        }
        let Some(width) = field_width(field, data.len().saturating_sub(cursor), &result.values)
        else {
            result.issues.push(format!(
                "cannot determine the width of '{}' at offset 0x{:04x}",
                field.label,
                base + cursor
            ));
            break;
        };
        if cursor + width > data.len() {
            result.issues.push(format!(
                "incomplete field '{}' at offset 0x{:04x}: expected {width} byte(s), found {}",
                field.label,
                base + cursor,
                data.len().saturating_sub(cursor)
            ));
            break;
        }
        let bytes = &data[cursor..cursor + width];
        let value = decode_field_value(field, bytes);
        result.fields.push(DecodedField {
            label: field.label.clone(),
            value: value.text.clone(),
            kind: field_kind(field),
            range: base + cursor..base + cursor + width,
        });
        if !field.name.is_empty() {
            result.values.insert(field.name.clone(), value);
            result
                .ranges
                .insert(field.name.clone(), base + cursor..base + cursor + width);
        }
        cursor += width;
    }

    result.consumed = cursor;
    result
}

fn field_width(
    field: &FieldDef,
    remaining: usize,
    values: &BTreeMap<String, FieldValue>,
) -> Option<usize> {
    if field.remaining {
        return Some(remaining);
    }
    if let Some(length) = field.length {
        return Some(length);
    }
    if let Some(source) = &field.length_from {
        return values
            .get(source)
            .and_then(|value| value.number)
            .and_then(|value| usize::try_from(value).ok());
    }
    if let Some(source) = &field.count_from
        && let Some(count) = values.get(source).and_then(|value| value.number)
    {
        return usize::try_from(count).ok();
    }
    field.kind.fixed_width()
}

fn field_kind(field: &FieldDef) -> FieldKind {
    match field.role {
        FieldRole::Length => FieldKind::Length,
        FieldRole::Target | FieldRole::Targets | FieldRole::Source | FieldRole::Broadcast => {
            FieldKind::Node
        }
        FieldRole::CommandClass => FieldKind::CommandClass,
        FieldRole::Value
        | FieldRole::Cluster
        | FieldRole::Profile
        | FieldRole::Group
        | FieldRole::SourceEndpoint
        | FieldRole::DestinationEndpoint
        | FieldRole::Payload
        | FieldRole::DestinationType
        | FieldRole::RxStatus
        | FieldRole::TxStatus
        | FieldRole::NodeInfo
        | FieldRole::CommandClassList
        | FieldRole::NodeBitmask
        | FieldRole::FunctionBitmask => FieldKind::Value,
    }
}

#[derive(Debug, Clone, Default)]
struct FieldValue {
    text: String,
    number: Option<u64>,
}

fn decode_field_value(field: &FieldDef, bytes: &[u8]) -> FieldValue {
    match field.kind {
        FieldType::Node
        | FieldType::Uint8
        | FieldType::Duration
        | FieldType::TxOptions
        | FieldType::CallbackId => numeric_value(field, bytes.first().copied().map(u64::from)),
        FieldType::Uint16 => {
            let value = (bytes.len() >= 2).then(|| field.endian.read_unsigned(&bytes[..2]));
            numeric_value(field, value)
        }
        FieldType::Uint32 => {
            let value = (bytes.len() >= 4).then(|| field.endian.read_unsigned(&bytes[..4]));
            numeric_value(field, value)
        }
        FieldType::Int => {
            if bytes.is_empty() || bytes.len() > 4 {
                FieldValue {
                    text: spaced_hex(bytes),
                    number: None,
                }
            } else {
                FieldValue {
                    text: field.endian.read_signed(bytes).to_string(),
                    number: None,
                }
            }
        }
        FieldType::Uint => numeric_value(field, Some(field.endian.read_unsigned(bytes))),
        FieldType::Bool => {
            let value = bytes.first().copied().unwrap_or_default();
            FieldValue {
                text: if value == 0 {
                    "Off".to_string()
                } else {
                    "On".to_string()
                },
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
        FieldType::Hex | FieldType::CommandClassData => FieldValue {
            text: spaced_hex(bytes),
            number: None,
        },
        FieldType::Text => FieldValue {
            text: String::from_utf8_lossy(bytes).into_owned(),
            number: None,
        },
        FieldType::NodeList => FieldValue {
            text: bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            number: Some(bytes.len() as u64),
        },
    }
}

fn numeric_value(field: &FieldDef, value: Option<u64>) -> FieldValue {
    let Some(value) = value else {
        return FieldValue {
            text: "?".to_string(),
            number: None,
        };
    };
    let text = match field.format {
        FieldFormat::Hex => {
            let width = field.kind.fixed_width().unwrap_or(1).max(1) * 2;
            format!("0x{value:0width$x}")
        }
        FieldFormat::Decimal | FieldFormat::Default => value.to_string(),
        _ => value.to_string(),
    };
    FieldValue {
        text,
        number: Some(value),
    }
}

// ---------------------------------------------------------------------------
// ZCL
// ---------------------------------------------------------------------------

struct ZclResult {
    fields: Vec<DecodedField>,
    summary: String,
    issues: Vec<String>,
}

fn decode_zcl(
    data: &[u8],
    base: usize,
    cluster_id: Option<u16>,
    definitions: &ZigbeeDefinitions,
) -> ZclResult {
    let mut result = ZclResult {
        fields: Vec::new(),
        summary: String::new(),
        issues: Vec::new(),
    };
    if data.len() < 3 {
        result
            .issues
            .push("incomplete ZCL frame header".to_string());
        return result;
    }

    let frame_control = data[0];
    let zcl_frame = &definitions.zcl_frame;
    let frame_type = frame_control & zcl_frame.frame_type_mask;
    let manufacturer_specific = frame_control & zcl_frame.manufacturer_specific_bit != 0;
    let direction = frame_control & zcl_frame.direction_to_client_bit != 0;
    let disable_default = frame_control & zcl_frame.disable_default_response_bit != 0;
    let mut cursor = 1;
    let manufacturer = if manufacturer_specific {
        let length = zcl_frame.manufacturer_code_length;
        if data.len() < cursor + length {
            result
                .issues
                .push("incomplete ZCL manufacturer code".to_string());
            return result;
        }
        let value = zcl_frame
            .manufacturer_code_endian
            .read_unsigned(&data[cursor..cursor + length]) as u16;
        cursor += length;
        Some(value)
    } else {
        None
    };
    if data.len() < cursor + 2 {
        result.issues.push("incomplete ZCL command".to_string());
        return result;
    }
    let sequence = data[cursor];
    let command_id = data[cursor + 1];
    cursor += 2;

    result.fields.push(DecodedField {
        label: "ZCL frame control".to_string(),
        value: format!(
            "0x{frame_control:02x} · {} · {}",
            if frame_type == definitions.zcl_frame.cluster_frame_type {
                "cluster-specific"
            } else {
                "general"
            },
            if direction {
                "server→client"
            } else {
                "client→server"
            }
        ),
        kind: FieldKind::Control,
        range: base..base + 1,
    });
    if let Some(manufacturer) = manufacturer {
        result.fields.push(DecodedField {
            label: "Manufacturer code".to_string(),
            value: format!("0x{manufacturer:04x}"),
            kind: FieldKind::Value,
            range: base + 1..base + 1 + definitions.zcl_frame.manufacturer_code_length,
        });
    }
    result.fields.push(DecodedField {
        label: "ZCL sequence".to_string(),
        value: sequence.to_string(),
        kind: FieldKind::Value,
        range: base + cursor - 2..base + cursor - 1,
    });

    let cluster = cluster_id.and_then(|id| definitions.cluster(id));
    let cluster_name = cluster
        .map(|cluster| cluster.name.as_str())
        .unwrap_or("ZCL");
    let cluster_command = (frame_type == definitions.zcl_frame.cluster_frame_type)
        .then(|| {
            cluster.and_then(|cluster| {
                cluster
                    .commands
                    .iter()
                    .find(|command| command.id == command_id)
            })
        })
        .flatten();
    let command_name = if frame_type == definitions.zcl_frame.cluster_frame_type {
        cluster_command.map(|command| command.name.as_str())
    } else {
        definitions
            .general_command(command_id)
            .map(|command| command.name.as_str())
    };

    result.fields.push(DecodedField {
        label: "ZCL command".to_string(),
        value: command_name
            .map(|name| format!("{name} (0x{command_id:02x})"))
            .unwrap_or_else(|| format!("0x{command_id:02x}")),
        kind: FieldKind::Command,
        range: base + cursor - 1..base + cursor,
    });
    if disable_default {
        result
            .fields
            .push(zcl_note("Default response", "disabled", base, cursor));
    }

    let payload = &data[cursor..];
    let payload_base = base + cursor;
    let mut summary = format!(
        "{cluster_name} · {}",
        command_name
            .map(str::to_string)
            .unwrap_or_else(|| format!("0x{command_id:02x}"))
    );

    if frame_type == definitions.zcl_frame.general_frame_type {
        decode_general_zcl(
            command_id,
            payload,
            payload_base,
            cluster,
            definitions,
            &mut result,
            &mut summary,
        );
    } else {
        decode_cluster_zcl(
            ClusterCommand {
                cluster_id,
                command_id,
                definition: cluster_command,
                direction,
            },
            payload,
            payload_base,
            &mut result,
            &mut summary,
        );
    }

    result.summary = summary;
    result
}

fn zcl_note(label: &str, value: &str, base: usize, _cursor: usize) -> DecodedField {
    DecodedField {
        label: label.to_string(),
        value: value.to_string(),
        kind: FieldKind::Control,
        range: base..base + 1,
    }
}

fn general_command_name(id: u8, definitions: &ZigbeeDefinitions) -> Option<&str> {
    definitions
        .general_command(id)
        .map(|command| command.name.as_str())
}

fn decode_general_zcl(
    command_id: u8,
    payload: &[u8],
    base: usize,
    cluster: Option<&ZclClusterDef>,
    definitions: &ZigbeeDefinitions,
    result: &mut ZclResult,
    summary: &mut String,
) {
    match command_id {
        0x00 => {
            let mut cursor = 0;
            let mut parts = Vec::new();
            while cursor + 2 <= payload.len() {
                let id = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
                parts.push(attribute_label(cluster, id));
                result.fields.push(attribute_field(
                    cluster,
                    id,
                    &format!("0x{id:04x}"),
                    base + cursor..base + cursor + 2,
                ));
                cursor += 2;
            }
            if !parts.is_empty() {
                summary.push_str(" · ");
                summary.push_str(&parts.join(", "));
            }
            if cursor < payload.len() {
                result
                    .fields
                    .push(raw_zcl_field(&payload[cursor..], base + cursor));
            }
        }
        0x02 | 0x03 | 0x05 => {
            let mut cursor = 0;
            while cursor + 3 <= payload.len() {
                let id = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
                let kind = ZclType::from_code(payload[cursor + 2]);
                cursor += 3;
                let Some((value, used)) = decode_zcl_value(kind, &payload[cursor..]) else {
                    break;
                };
                result.fields.push(attribute_field(
                    cluster,
                    id,
                    &value,
                    base + cursor - 3..base + cursor + used,
                ));
                summary.push_str(&format!(" · {}={value}", attribute_label(cluster, id)));
                cursor += used;
            }
            if cursor < payload.len() {
                result
                    .fields
                    .push(raw_zcl_field(&payload[cursor..], base + cursor));
            }
        }
        0x01 => {
            let mut cursor = 0;
            while cursor + 3 <= payload.len() {
                let id = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
                let status = payload[cursor + 2];
                cursor += 3;
                if status == 0 && cursor < payload.len() {
                    let kind = ZclType::from_code(payload[cursor]);
                    cursor += 1;
                    if let Some((value, used)) = decode_zcl_value(kind, &payload[cursor..]) {
                        result.fields.push(attribute_field(
                            cluster,
                            id,
                            &value,
                            base + cursor - 1..base + cursor + used,
                        ));
                        summary.push_str(&format!(" · {}={value}", attribute_label(cluster, id)));
                        cursor += used;
                        continue;
                    }
                }
                result.fields.push(attribute_field(
                    cluster,
                    id,
                    &zcl_status_name(status),
                    base + cursor - 3..base + cursor,
                ));
            }
        }
        0x0a => {
            let mut cursor = 0;
            while cursor + 3 <= payload.len() {
                let id = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
                let kind = ZclType::from_code(payload[cursor + 2]);
                cursor += 3;
                let Some((value, used)) = decode_zcl_value(kind, &payload[cursor..]) else {
                    break;
                };
                result.fields.push(attribute_field(
                    cluster,
                    id,
                    &value,
                    base + cursor - 3..base + cursor + used,
                ));
                summary.push_str(&format!(" · {}={value}", attribute_label(cluster, id)));
                cursor += used;
            }
            if cursor < payload.len() {
                result
                    .fields
                    .push(raw_zcl_field(&payload[cursor..], base + cursor));
            }
        }
        0x04 => {
            let mut cursor = 0;
            while cursor + 3 <= payload.len() {
                let status = payload[cursor];
                let id = u16::from_le_bytes([payload[cursor + 1], payload[cursor + 2]]);
                result.fields.push(attribute_field(
                    cluster,
                    id,
                    &zcl_status_name(status),
                    base + cursor..base + cursor + 3,
                ));
                cursor += 3;
            }
            if cursor < payload.len() {
                result
                    .fields
                    .push(raw_zcl_field(&payload[cursor..], base + cursor));
            }
        }
        0x0b => {
            if payload.len() >= 2 {
                summary.push_str(&format!(
                    " · {} · {}",
                    general_command_name(payload[0], definitions)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("0x{:02x}", payload[0])),
                    zcl_status_name(payload[1])
                ));
            }
            result.fields.push(raw_zcl_field(payload, base));
        }
        0x0c => {
            if payload.len() >= 3 {
                let start = u16::from_le_bytes([payload[0], payload[1]]);
                let max = payload[2];
                summary.push_str(&format!(" · start 0x{start:04x} · max {max}"));
            }
            result.fields.push(raw_zcl_field(payload, base));
        }
        0x0d => {
            if !payload.is_empty() {
                summary.push_str(&format!(
                    " · {}",
                    if payload[0] == 0 { "complete" } else { "more" }
                ));
            }
            result.fields.push(raw_zcl_field(payload, base));
        }
        _ => {
            if !payload.is_empty() {
                result.fields.push(raw_zcl_field(payload, base));
            }
        }
    }
}

struct ClusterCommand<'a> {
    cluster_id: Option<u16>,
    command_id: u8,
    definition: Option<&'a ZclCommandDef>,
    direction: bool,
}

fn decode_cluster_zcl(
    command: ClusterCommand<'_>,
    payload: &[u8],
    base: usize,
    result: &mut ZclResult,
    summary: &mut String,
) {
    let (layout, template) = command
        .definition
        .map(|definition| {
            if command.direction {
                (
                    definition.response_fields.as_slice(),
                    definition.response_summary.as_deref(),
                )
            } else {
                (definition.fields.as_slice(), definition.summary.as_deref())
            }
        })
        .unwrap_or((&[], None));
    if !layout.is_empty() {
        let decoded = decode_layout(payload, layout, base);
        result.fields.extend(decoded.fields);
        result.issues.extend(decoded.issues);
        if let Some(template) = template {
            let rendered = render_summary(template, &decoded.values);
            if !rendered.is_empty() {
                summary.push_str(" · ");
                summary.push_str(&rendered);
            }
        }
        if decoded.consumed < payload.len() {
            let rest = &payload[decoded.consumed..];
            if !rest.is_empty() {
                result
                    .fields
                    .push(raw_zcl_field(rest, base + decoded.consumed));
            }
        }
        return;
    }
    match (command.cluster_id, command.command_id) {
        // The command name already carries these one-word labels. Appending it
        // again produced summaries such as `On/Off · On · On`.
        (Some(0x0006), 0x00..=0x02) => {}
        (Some(0x0008), 0x00 | 0x04) => {
            if let Some(level) = payload.first() {
                summary.push_str(&format!(" · level {level}"));
                result.fields.push(DecodedField {
                    label: "Level".to_string(),
                    value: level.to_string(),
                    kind: FieldKind::Value,
                    range: base..base + 1,
                });
                if payload.len() >= 3 {
                    let transition = u16::from_le_bytes([payload[1], payload[2]]);
                    summary.push_str(&format!(" · transition {transition}"));
                    result.fields.push(DecodedField {
                        label: "Transition time".to_string(),
                        value: transition.to_string(),
                        kind: FieldKind::Value,
                        range: base + 1..base + 3,
                    });
                }
            }
        }
        (Some(0x0008), 0x01) => {
            if payload.len() >= 2 {
                summary.push_str(&format!(
                    " · {} · rate {}",
                    if payload[0] == 0 { "up" } else { "down" },
                    payload[1]
                ));
            }
            result.fields.push(raw_zcl_field(payload, base));
        }
        (Some(0x0008), 0x02) => {
            if payload.len() >= 4 {
                summary.push_str(&format!(
                    " · {} · step {} · transition {}",
                    if payload[0] == 0 { "up" } else { "down" },
                    payload[1],
                    u16::from_le_bytes([payload[2], payload[3]])
                ));
            }
            result.fields.push(raw_zcl_field(payload, base));
        }
        (Some(0x0008), 0x03) => {}
        (Some(0x0000), 0x00) => {}
        (Some(0x0003), 0x00) => {
            if payload.len() >= 2 {
                summary.push_str(&format!(
                    " · identify {}s",
                    u16::from_le_bytes([payload[0], payload[1]])
                ));
            }
        }
        (Some(0x0003), 0x01) => {}
        _ => {
            if !payload.is_empty() {
                result.fields.push(raw_zcl_field(payload, base));
            }
        }
    }
}

fn attribute_label(cluster: Option<&ZclClusterDef>, id: u16) -> String {
    cluster
        .and_then(|cluster| {
            cluster
                .attributes
                .iter()
                .find(|attribute| attribute.id == id)
        })
        .map(|attribute| attribute.name.clone())
        .unwrap_or_else(|| format!("0x{id:04x}"))
}

fn attribute_field(
    cluster: Option<&ZclClusterDef>,
    id: u16,
    value: &str,
    range: Range<usize>,
) -> DecodedField {
    DecodedField {
        label: attribute_label(cluster, id),
        value: value.to_string(),
        kind: FieldKind::Value,
        range,
    }
}

fn raw_zcl_field(payload: &[u8], base: usize) -> DecodedField {
    DecodedField {
        label: "ZCL payload".to_string(),
        value: spaced_hex(payload),
        kind: FieldKind::Value,
        range: base..base + payload.len(),
    }
}

fn zcl_status_name(status: u8) -> String {
    match status {
        0x00 => "success".to_string(),
        0x01 => "failure".to_string(),
        0x80 => "malformed command".to_string(),
        0x81 => "unsupported cluster".to_string(),
        0x82 => "unsupported general command".to_string(),
        0x83 => "unsupported manufacturer cluster command".to_string(),
        0x84 => "unsupported manufacturer general command".to_string(),
        0x85 => "invalid field".to_string(),
        0x86 => "unsupported attribute".to_string(),
        0x87 => "invalid value".to_string(),
        0x88 => "read only".to_string(),
        0x89 => "insufficient space".to_string(),
        0x8a => "duplicate exists".to_string(),
        0x8b => "not found".to_string(),
        0x8c => "unreportable attribute".to_string(),
        0x8d => "invalid data type".to_string(),
        0x8e => "invalid selector".to_string(),
        0x8f => "write only".to_string(),
        0x90 => "inconsistent start".to_string(),
        0x91 => "defined out of range".to_string(),
        0x92 => "hardware failure".to_string(),
        0x93 => "software failure".to_string(),
        0x94 => "calibration error".to_string(),
        0x95 => "unsupported cluster command".to_string(),
        _ => format!("status 0x{status:02x}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZclType {
    Bool,
    Uint8,
    Uint16,
    Uint24,
    Uint32,
    Int8,
    Int16,
    Int24,
    Int32,
    Enum8,
    Enum16,
    Bitmap8,
    Bitmap16,
    String,
    Hex,
    #[serde(other)]
    Unknown,
}

impl ZclType {
    fn from_code(code: u8) -> Self {
        match code {
            0x10 => Self::Bool,
            0x20 => Self::Uint8,
            0x21 => Self::Uint16,
            0x22 => Self::Uint24,
            0x23 => Self::Uint32,
            0x28 => Self::Int8,
            0x29 => Self::Int16,
            0x2a => Self::Int24,
            0x2b => Self::Int32,
            0x30 => Self::Enum8,
            0x31 => Self::Enum16,
            0x18 => Self::Bitmap8,
            0x19 => Self::Bitmap16,
            0x42 => Self::String,
            0x41 => Self::Hex,
            _ => Self::Unknown,
        }
    }
}

fn decode_zcl_value(kind: ZclType, data: &[u8]) -> Option<(String, usize)> {
    match kind {
        ZclType::Bool => {
            let value = *data.first()?;
            Some((
                if value == 0 {
                    "Off".to_string()
                } else {
                    "On".to_string()
                },
                1,
            ))
        }
        ZclType::Uint8 | ZclType::Enum8 | ZclType::Bitmap8 => {
            let value = *data.first()?;
            Some((value.to_string(), 1))
        }
        ZclType::Uint16 | ZclType::Enum16 | ZclType::Bitmap16 => {
            if data.len() < 2 {
                return None;
            }
            Some((u16::from_le_bytes([data[0], data[1]]).to_string(), 2))
        }
        ZclType::Uint24 => {
            if data.len() < 3 {
                return None;
            }
            Some((
                (u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16))
                    .to_string(),
                3,
            ))
        }
        ZclType::Uint32 => {
            if data.len() < 4 {
                return None;
            }
            Some((
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]).to_string(),
                4,
            ))
        }
        ZclType::Int8 => Some(((*data.first()? as i8).to_string(), 1)),
        ZclType::Int16 => {
            if data.len() < 2 {
                return None;
            }
            Some((i16::from_le_bytes([data[0], data[1]]).to_string(), 2))
        }
        ZclType::Int24 => {
            if data.len() < 3 {
                return None;
            }
            let raw = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
            let value = if raw & 0x800000 != 0 {
                (raw | 0xff000000) as i32
            } else {
                raw as i32
            };
            Some((value.to_string(), 3))
        }
        ZclType::Int32 => {
            if data.len() < 4 {
                return None;
            }
            Some((
                i32::from_le_bytes([data[0], data[1], data[2], data[3]]).to_string(),
                4,
            ))
        }
        ZclType::String | ZclType::Hex => {
            let length = usize::from(*data.first()?);
            if data.len() < 1 + length {
                return None;
            }
            let bytes = &data[1..1 + length];
            let value = if matches!(kind, ZclType::String) {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                spaced_hex(bytes)
            };
            Some((value, 1 + length))
        }
        ZclType::Unknown => None,
    }
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZigbeeDefinitions {
    pub version: u32,
    pub protocol: String,
    pub stack: String,
    #[serde(default)]
    pub commands: Vec<EzspCommandDef>,
    #[serde(default)]
    pub general_commands: Vec<ZclGeneralCommandDef>,
    #[serde(default)]
    pub zcl_frame: ZclFrameDef,
    #[serde(default)]
    pub clusters: Vec<ZclClusterDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EzspCommandDef {
    pub id: u16,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// Selected when the Send message dialog opens for this protocol.
    #[serde(default)]
    pub default_operation: bool,
    /// Highlight the operation in the Operation picker.
    #[serde(default)]
    pub emphasize: bool,
    #[serde(default)]
    pub incoming_only: bool,
    #[serde(default)]
    pub no_request_parameters: bool,
    #[serde(default)]
    pub fields: Vec<LayoutField>,
    #[serde(default)]
    pub request_summary: Option<String>,
    #[serde(default)]
    pub response_fields: Vec<LayoutField>,
    #[serde(default)]
    pub response_summary: Option<String>,
}

fn default_priority() -> i32 {
    1000
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZclDirection {
    ClientToServer,
    ServerToClient,
    #[default]
    Both,
}

impl ZclDirection {
    pub fn matches(self, to_client: bool) -> bool {
        match self {
            Self::ClientToServer => !to_client,
            Self::ServerToClient => to_client,
            Self::Both => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZclGeneralCommandDef {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: ZclDirection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZclFrameDef {
    #[serde(default = "default_zcl_frame_type_mask")]
    pub frame_type_mask: u8,
    #[serde(default = "default_zcl_general_frame_type")]
    pub general_frame_type: u8,
    #[serde(default = "default_zcl_cluster_frame_type")]
    pub cluster_frame_type: u8,
    #[serde(default = "default_zcl_direction_bit")]
    pub direction_to_client_bit: u8,
    #[serde(default = "default_zcl_disable_default_response_bit")]
    pub disable_default_response_bit: u8,
    #[serde(default = "default_zcl_manufacturer_bit")]
    pub manufacturer_specific_bit: u8,
    #[serde(default = "default_zcl_frame_control_mask")]
    pub frame_control_mask: u8,
    #[serde(default = "default_zcl_manufacturer_code_length")]
    pub manufacturer_code_length: usize,
    #[serde(default = "default_zcl_manufacturer_code_endian")]
    pub manufacturer_code_endian: Endian,
}

impl Default for ZclFrameDef {
    fn default() -> Self {
        Self {
            frame_type_mask: default_zcl_frame_type_mask(),
            general_frame_type: default_zcl_general_frame_type(),
            cluster_frame_type: default_zcl_cluster_frame_type(),
            direction_to_client_bit: default_zcl_direction_bit(),
            disable_default_response_bit: default_zcl_disable_default_response_bit(),
            manufacturer_specific_bit: default_zcl_manufacturer_bit(),
            frame_control_mask: default_zcl_frame_control_mask(),
            manufacturer_code_length: default_zcl_manufacturer_code_length(),
            manufacturer_code_endian: default_zcl_manufacturer_code_endian(),
        }
    }
}

fn default_zcl_frame_type_mask() -> u8 {
    0x03
}

fn default_zcl_general_frame_type() -> u8 {
    0
}

fn default_zcl_cluster_frame_type() -> u8 {
    1
}

fn default_zcl_direction_bit() -> u8 {
    0x08
}

fn default_zcl_disable_default_response_bit() -> u8 {
    0x10
}

fn default_zcl_manufacturer_bit() -> u8 {
    0x04
}

fn default_zcl_frame_control_mask() -> u8 {
    0x1f
}

fn default_zcl_manufacturer_code_length() -> usize {
    2
}

fn default_zcl_manufacturer_code_endian() -> Endian {
    Endian::Little
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZclClusterDef {
    pub id: u16,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub commands: Vec<ZclCommandDef>,
    #[serde(default)]
    pub attributes: Vec<ZclAttributeDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZclCommandDef {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Command payload fields for client-to-server frames.
    #[serde(default)]
    pub fields: Vec<LayoutField>,
    /// Optional one-line detail template rendered from the decoded fields.
    #[serde(default)]
    pub summary: Option<String>,
    /// Command payload fields for server-to-client frames.
    #[serde(default)]
    pub response_fields: Vec<LayoutField>,
    #[serde(default)]
    pub response_summary: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZclAttributeDef {
    pub id: u16,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ZclType,
    #[serde(default)]
    pub description: Option<String>,
}

impl ZigbeeDefinitions {
    pub fn command(&self, id: u16) -> Option<&EzspCommandDef> {
        self.commands.iter().find(|command| command.id == id)
    }

    pub fn general_command(&self, id: u8) -> Option<&ZclGeneralCommandDef> {
        self.general_commands
            .iter()
            .find(|command| command.id == id)
    }

    pub fn cluster(&self, id: u16) -> Option<&ZclClusterDef> {
        self.clusters.iter().find(|cluster| cluster.id == id)
    }

    /// Byte offset of a named field in a canonical EZSP frame.
    pub fn field_offset(&self, command_id: u16, field_name: &str) -> Option<usize> {
        let command = self.command(command_id)?;
        for layout in [&command.fields, &command.response_fields] {
            if let Some(offset) = layout_field_offset(layout, |field| field.name == field_name) {
                return Some(EZSP_HEADER_LEN + offset);
            }
        }
        None
    }

    /// Byte offset and definition of a semantic role in a canonical EZSP frame.
    pub fn field_layout_by_role(
        &self,
        command_id: u16,
        role: FieldRole,
    ) -> Option<(usize, &FieldDef)> {
        let command = self.command(command_id)?;
        for layout in [&command.fields, &command.response_fields] {
            if let Some((offset, field)) = layout_field_layout(layout, |field| field.role == role) {
                return Some((EZSP_HEADER_LEN + offset, field));
            }
        }
        None
    }

    /// Byte offset and definition of a filter slot in a canonical EZSP frame.
    pub fn field_layout_by_filter_slot(
        &self,
        command_id: u16,
        slot: FilterSlot,
    ) -> Option<(usize, &FieldDef)> {
        let command = self.command(command_id)?;
        for layout in [&command.fields, &command.response_fields] {
            if let Some((offset, field)) =
                layout_field_layout(layout, |field| field.filter_slot == Some(slot))
            {
                return Some((EZSP_HEADER_LEN + offset, field));
            }
        }
        None
    }

    /// Byte offset of a semantic role in a canonical EZSP frame.
    pub fn field_offset_by_role(&self, command_id: u16, role: FieldRole) -> Option<usize> {
        self.field_layout_by_role(command_id, role)
            .map(|(offset, _)| offset)
    }

    /// First request field with a semantic role. Builder fields come from the
    /// request layout, never from a response layout.
    pub fn request_field_by_role(&self, command_id: u16, role: FieldRole) -> Option<&FieldDef> {
        let command = self.command(command_id)?;
        layout_field_by_role(&command.fields, role)
    }

    pub fn request_field_name_by_role(&self, command_id: u16, role: FieldRole) -> Option<&str> {
        self.request_field_by_role(command_id, role)
            .map(|field| field.name.as_str())
    }

    pub fn request_has_role(&self, command_id: u16, role: FieldRole) -> bool {
        self.command(command_id)
            .is_some_and(|command| layout_has_role(&command.fields, role))
    }
}

static INSTALLED: OnceLock<RwLock<Option<Arc<ZigbeeDefinitions>>>> = OnceLock::new();

fn installed() -> &'static RwLock<Option<Arc<ZigbeeDefinitions>>> {
    INSTALLED.get_or_init(|| RwLock::new(None))
}

pub fn bundled() -> &'static ZigbeeDefinitions {
    static DEFINITIONS: OnceLock<ZigbeeDefinitions> = OnceLock::new();
    DEFINITIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/zigbee.json"))
            .expect("bundled zigbee.json must be valid")
    })
}

fn bundled_shared() -> Arc<ZigbeeDefinitions> {
    static SHARED: OnceLock<Arc<ZigbeeDefinitions>> = OnceLock::new();
    Arc::clone(SHARED.get_or_init(|| Arc::new(bundled().clone())))
}

pub fn install(json: &str) -> Result<(), String> {
    let definitions: ZigbeeDefinitions = serde_json::from_str(json)
        .map_err(|error| format!("invalid ZigBee definitions: {error}"))?;
    if definitions.protocol != "zigbee" {
        return Err(format!(
            "invalid ZigBee definitions: expected protocol 'zigbee', found '{}'",
            definitions.protocol
        ));
    }
    if definitions.stack != "ezsp" {
        return Err(format!(
            "invalid ZigBee definitions: expected stack 'ezsp', found '{}'",
            definitions.stack
        ));
    }
    *installed()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(definitions));
    Ok(())
}

pub fn current() -> Arc<ZigbeeDefinitions> {
    installed()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(bundled_shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_known_ash_vectors() {
        assert_eq!(crc16(&[0xc0]), 0x38bc);
        assert_eq!(crc16(&[0x81]), 0x6059);
        assert_eq!(crc16(&[0xa1]), 0x443b);
        assert_eq!(crc16(&[0xc1, 0x02, 0x0b]), 0x0a52);
    }

    #[test]
    fn link_frames_round_trip() {
        let ack = encode_control_frame(0x81, &[]);
        assert_eq!(ack, [0x81, 0x60, 0x59, 0x7e]);
        let frame = decode_link_frame(&ack).expect("ack");
        assert_eq!(frame.kind, FrameKind::Ack);
        assert_eq!(frame.summary, "ASH ACK · ack 1");
        assert!(is_link_frame(&ack));

        let rst = encode_control_frame(0xc0, &[]);
        assert_eq!(rst, [0xc0, 0x38, 0xbc, 0x7e]);
        let frame = decode_link_frame(&rst).expect("rst");
        assert_eq!(frame.kind, FrameKind::Reset);
    }

    #[test]
    fn builds_canonical_ezsp_frames() {
        assert_eq!(
            build_request(0, EZSP_VERSION, &[4]),
            [0x00, 0x00, 0x01, 0x00, 0x00, 0x04]
        );
    }

    #[test]
    fn bundled_definitions_parse() {
        let definitions = bundled();
        assert_eq!(definitions.protocol, "zigbee");
        assert_eq!(definitions.stack, "ezsp");
        assert!(definitions.command(EZSP_VERSION).is_some());
        assert!(definitions.command(EZSP_INCOMING_MESSAGE_HANDLER).is_some());
        assert!(definitions.cluster(0x0006).is_some());
        assert_eq!(definitions.general_commands.len(), 21);
        assert_eq!(
            definitions
                .general_command(0x0a)
                .map(|command| command.name.as_str()),
            Some("Report attributes")
        );
        assert_eq!(
            definitions
                .general_command(0x0a)
                .map(|command| command.direction),
            Some(ZclDirection::ServerToClient)
        );
        assert_eq!(definitions.zcl_frame.general_frame_type, 0);
        assert_eq!(definitions.zcl_frame.cluster_frame_type, 1);
    }

    #[test]
    fn filter_slots_resolve_offsets() {
        use crate::definitions::FilterSlot;

        let definitions = bundled();
        assert_eq!(
            definitions
                .field_layout_by_filter_slot(EZSP_SEND_UNICAST, FilterSlot::Network)
                .map(|(offset, field)| (offset, field.name.as_str())),
            Some((6, "indexOrDestination"))
        );
        assert_eq!(
            definitions
                .field_layout_by_filter_slot(EZSP_SEND_BROADCAST, FilterSlot::Network)
                .map(|(offset, field)| (offset, field.name.as_str())),
            Some((5, "destination"))
        );
        assert_eq!(
            definitions
                .field_layout_by_filter_slot(EZSP_INCOMING_MESSAGE_HANDLER, FilterSlot::Endpoint)
                .map(|(offset, field)| (offset, field.name.as_str())),
            Some((10, "sourceEndpoint"))
        );
        assert_eq!(
            definitions
                .field_layout_by_filter_slot(EZSP_SEND_MULTICAST, FilterSlot::Payload)
                .map(|(offset, _)| offset),
            Some(20)
        );
    }

    #[test]
    fn decodes_ash_link_and_ezsp_frames() {
        let ack = [0x81, 0x60, 0x59, 0x7e];
        let decoded = decode(&ack);
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].kind, FrameKind::Ack);
        assert!(decoded.summary.contains("ASH ACK"));

        let version = [0x00, 0x80, 0x01, 0x00, 0x00, 0x04, 0x02, 0x10, 0x54];
        let decoded = decode(&version);
        assert_eq!(decoded.frames.len(), 1);
        assert!(decoded.summary.contains("version"));
        assert!(decoded.summary.contains("EZSP v4"));
    }

    #[test]
    fn on_off_summary_does_not_repeat_the_command_name() {
        let zcl = [0x01, 0x01, 0x01]; // cluster-specific, sequence 1, On
        let mut params = vec![
            0x00,
            0x34,
            0x12,
            0x04,
            0x01,
            0x06,
            0x00,
            0x01,
            0x01,
            0x00,
            0x00,
            0x00,
            0x00,
            0x01,
            0x01,
            zcl.len() as u8,
        ];
        params.extend(zcl);
        let frame = build_request(7, EZSP_SEND_UNICAST, &params);
        let decoded = decode(&frame);
        assert!(
            decoded.summary.contains("On/Off · On"),
            "{}",
            decoded.summary
        );
        assert!(!decoded.summary.contains("On · On"), "{}", decoded.summary);
    }

    #[test]
    fn decodes_zcl_level_fields_from_send_unicast() {
        let zcl = [0x01, 0x05, 0x00, 0x80, 0x02, 0x00];
        let mut params = vec![
            0x00, // direct
            0x34,
            0x12, // destination
            0x04,
            0x01, // HA profile
            0x08,
            0x00, // Level cluster
            0x01, // source endpoint
            0x01, // destination endpoint
            0x00,
            0x00, // APS options
            0x00,
            0x00, // group
            0x01, // APS sequence
            0x01, // message tag
            zcl.len() as u8,
        ];
        params.extend(zcl);
        let frame = build_request(7, EZSP_SEND_UNICAST, &params);
        let decoded = decode(&frame);
        assert!(decoded.summary.contains("level 128"), "{}", decoded.summary);
        let fields = &decoded.frames[0].fields;
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Level" && field.value == "128")
        );
        assert!(
            fields
                .iter()
                .any(|field| { field.label.starts_with("Transition time") && field.value == "2" })
        );
        assert!(fields.iter().any(|field| {
            field.label == "ZCL command" && field.value.contains("Move to level")
        }));
    }

    #[test]
    fn decodes_zcl_on_with_timed_off_fields() {
        let zcl = [0x01, 0x09, 0x42, 0x0a, 0x00, 0x1e, 0x00];
        let mut params = vec![
            0x00,
            0x34,
            0x12,
            0x04,
            0x01,
            0x06,
            0x00,
            0x01,
            0x01,
            0x00,
            0x00,
            0x00,
            0x00,
            0x01,
            0x01,
            zcl.len() as u8,
        ];
        params.extend(zcl);
        let frame = build_request(8, EZSP_SEND_UNICAST, &params);
        let decoded = decode(&frame);
        let fields = &decoded.frames[0].fields;
        assert!(
            fields
                .iter()
                .any(|field| field.label == "On time (1/10 s)" && field.value == "10")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.label == "Off wait time (1/10 s)" && field.value == "30")
        );
        assert!(decoded.summary.contains("on for 10"), "{}", decoded.summary);
    }

    #[test]
    fn locates_named_fields_in_fixed_layouts() {
        assert_eq!(field_offset(EZSP_SEND_UNICAST, "groupId"), Some(16));
        assert_eq!(field_offset(EZSP_SEND_BROADCAST, "groupId"), Some(15));
        assert_eq!(field_offset(EZSP_SEND_MULTICAST, "groupId"), Some(13));
        assert_eq!(
            field_offset(EZSP_INCOMING_MESSAGE_HANDLER, "groupId"),
            Some(14)
        );
        assert_eq!(field_offset(EZSP_MESSAGE_SENT_HANDLER, "groupId"), Some(16));
        assert_eq!(field_offset(EZSP_SEND_UNICAST, "clusterId"), Some(10));
        assert_eq!(
            field_offset(EZSP_INCOMING_MESSAGE_HANDLER, "sender"),
            Some(19)
        );
    }

    #[test]
    fn locates_message_contents_in_fixed_layouts() {
        assert_eq!(message_contents_offset(EZSP_SEND_UNICAST), Some(21));
        assert_eq!(message_contents_offset(EZSP_SEND_BROADCAST), Some(21));
        assert_eq!(message_contents_offset(EZSP_SEND_MULTICAST), Some(20));
        assert_eq!(
            message_contents_offset(EZSP_INCOMING_MESSAGE_HANDLER),
            Some(24)
        );
        assert_eq!(message_contents_offset(EZSP_MESSAGE_SENT_HANDLER), Some(22));
    }

    #[test]
    fn semantic_roles_resolve_offsets_and_builder_fields() {
        use crate::definitions::FieldRole;

        let definitions = bundled();
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_UNICAST, FieldRole::Target),
            Some(6)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_UNICAST, FieldRole::Cluster),
            Some(10)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_UNICAST, FieldRole::DestinationEndpoint),
            Some(13)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_UNICAST, FieldRole::Group),
            Some(16)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_UNICAST, FieldRole::Payload),
            Some(21)
        );

        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_BROADCAST, FieldRole::Broadcast),
            Some(5)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_BROADCAST, FieldRole::Target),
            None
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_SEND_MULTICAST, FieldRole::Group),
            Some(13)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_INCOMING_MESSAGE_HANDLER, FieldRole::Source),
            Some(19)
        );
        assert_eq!(
            definitions
                .field_offset_by_role(EZSP_INCOMING_MESSAGE_HANDLER, FieldRole::SourceEndpoint),
            Some(10)
        );
        assert_eq!(
            definitions.field_offset_by_role(EZSP_PERMIT_JOINING, FieldRole::Payload),
            None
        );

        assert_eq!(
            definitions.request_field_name_by_role(EZSP_SEND_UNICAST, FieldRole::Target),
            Some("indexOrDestination")
        );
        assert_eq!(
            definitions.request_field_name_by_role(EZSP_SEND_BROADCAST, FieldRole::Target),
            None
        );
        assert_eq!(
            definitions.request_field_name_by_role(EZSP_SEND_UNICAST, FieldRole::DestinationType),
            Some("type")
        );
    }

    #[test]
    fn decodes_zcl_on_off_report() {
        let frame = [
            0x02, 0x80, 0x01, 0x45, 0x00, 0x00, 0x04, 0x01, 0x06, 0x00, 0x01, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x01, 0xff, 0xce, 0x34, 0x12, 0x00, 0x00, 0x07, 0x18, 0x05, 0x0a, 0x00,
            0x00, 0x10, 0x01,
        ];
        let decoded = decode(&frame);
        assert_eq!(decoded.frames.len(), 1);
        assert!(decoded.summary.contains("On/Off"));
        assert!(decoded.summary.contains("OnOff=On"));
    }
}
