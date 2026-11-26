//! Pure display helpers and message-buffer logic.
//!
//! These functions have no egui dependency so they can be unit-tested without a
//! window. The UI layer owns colours and layout.

use chrono::{DateTime, Local, Utc};
use hodeco_codec::entities;
use hodeco_codec::hex::spaced_hex;
use hodeco_codec::zigbee;
use hodeco_codec::zwave;
use hodeco_contract::domain::{
    Message, MessageState, PayloadExpr, PayloadOffset, ProtocolInfo, ServiceInfo, ServiceRef,
    TransportInfo,
};
pub const EXPECTED_PROTOCOL_VERSION: &str = "1.0";
/// Normal page size for the bounded local message list.
pub const MESSAGE_PAGE_LIMIT: u32 = 50;
/// Maximum number of messages kept in the local list.
pub const MESSAGE_LIST_CAP: usize = 1_000;
/// Maximum number of IDs resolved in one notification batch.
pub const MESSAGE_NOTIFICATION_BATCH: u32 = 1_000;

/// Maximum number of payload bytes decoded for compact summaries and local
/// search. The full payload remains available in Hex and JSON views.
pub const SUMMARY_DECODE_LIMIT: usize = hodeco_codec::SUMMARY_DECODE_LIMIT;
pub const SUMMARY_MAX_CHARS: usize = hodeco_codec::SUMMARY_MAX_CHARS;

/// Character encoding used by the payload search input and the Hex view text
/// column. UTF-16 is exposed as two explicit modes so the byte order is never
/// guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextEncoding {
    #[default]
    Ascii,
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub const ALL: [Self; 4] = [Self::Ascii, Self::Utf8, Self::Utf16Le, Self::Utf16Be];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
        }
    }

    /// Decodes one row chunk for the Hex view text column.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Ascii => bytes
                .iter()
                .map(|byte| printable_ascii(*byte))
                .collect::<String>(),
            Self::Utf8 => String::from_utf8_lossy(bytes)
                .chars()
                .map(printable_char)
                .collect(),
            Self::Utf16Le => decode_utf16(bytes, true),
            Self::Utf16Be => decode_utf16(bytes, false),
        }
    }

    /// Decodes just the characters whose first byte starts in `start..end`.
    ///
    /// Reading a little past `end` keeps a multi-byte character intact when it
    /// straddles the 16-byte row boundary. The character belongs to the row
    /// where it starts, so neighbouring rows never show a replacement glyph.
    pub fn decode_row(self, bytes: &[u8], start: usize, end: usize) -> String {
        if start >= end || start >= bytes.len() {
            return String::new();
        }
        let window_end = (end + 3).min(bytes.len());
        let encoding = if start == 0 {
            match self {
                Self::Utf16Le if bytes.starts_with(&[0xfe, 0xff]) => Self::Utf16Be,
                Self::Utf16Be if bytes.starts_with(&[0xff, 0xfe]) => Self::Utf16Le,
                _ => self,
            }
        } else {
            self
        };
        let mut window_start = start;
        match encoding {
            Self::Utf8 => {
                while window_start < window_end && bytes[window_start] & 0xc0 == 0x80 {
                    window_start += 1;
                }
            }
            Self::Utf16Le | Self::Utf16Be => {
                if window_start + 1 < window_end {
                    let little_endian = encoding == Self::Utf16Le;
                    let unit = if little_endian {
                        u16::from_le_bytes([bytes[window_start], bytes[window_start + 1]])
                    } else {
                        u16::from_be_bytes([bytes[window_start], bytes[window_start + 1]])
                    };
                    if (0xdc00..=0xdfff).contains(&unit) {
                        window_start += 2;
                    }
                }
            }
            Self::Ascii => {}
        }
        let window = &bytes[window_start..window_end];
        encoding
            .chars_with_offsets(window)
            .into_iter()
            .filter(|(offset, character)| window_start + *offset < end && *character != '\u{feff}')
            .map(|(_, character)| character)
            .collect()
    }

    fn chars_with_offsets(self, bytes: &[u8]) -> Vec<(usize, char)> {
        match self {
            Self::Ascii => bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| (index, printable_ascii(*byte)))
                .collect(),
            Self::Utf8 => utf8_chars_lossy(bytes),
            Self::Utf16Le => utf16_chars_lossy(bytes, true),
            Self::Utf16Be => utf16_chars_lossy(bytes, false),
        }
    }
}

fn printable_ascii(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        char::from(byte)
    } else {
        '.'
    }
}

fn printable_char(character: char) -> char {
    if character.is_control() {
        '.'
    } else {
        character
    }
}

fn utf8_chars_lossy(bytes: &[u8]) -> Vec<(usize, char)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match std::str::from_utf8(&bytes[index..]) {
            Ok(text) => {
                result.extend(
                    text.char_indices()
                        .map(|(offset, character)| (index + offset, printable_char(character))),
                );
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let text = std::str::from_utf8(&bytes[index..index + valid])
                        .expect("valid_up_to guarantees valid UTF-8");
                    result
                        .extend(text.char_indices().map(|(offset, character)| {
                            (index + offset, printable_char(character))
                        }));
                    index += valid;
                }
                if index >= bytes.len() {
                    break;
                }
                result.push((index, '.'));
                let invalid = error.error_len().unwrap_or(1).max(1);
                index = (index + invalid).min(bytes.len());
            }
        }
    }
    result
}

fn utf16_chars_lossy(bytes: &[u8], little_endian: bool) -> Vec<(usize, char)> {
    let read = |index: usize| {
        let pair = [bytes[index], bytes[index + 1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    };
    let mut result = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        let unit = read(index);
        if (0xd800..=0xdbff).contains(&unit)
            && index + 3 < bytes.len()
            && (0xdc00..=0xdfff).contains(&read(index + 2))
        {
            let low = read(index + 2);
            let code = 0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
            result.push((
                index,
                char::from_u32(code).map(printable_char).unwrap_or('.'),
            ));
            index += 4;
            continue;
        }
        result.push((
            index,
            char::from_u32(u32::from(unit))
                .map(printable_char)
                .unwrap_or('.'),
        ));
        index += 2;
    }
    if index < bytes.len() {
        result.push((index, '.'));
    }
    result
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let (bytes, little_endian) = if bytes.starts_with(&[0xff, 0xfe]) {
        (&bytes[2..], true)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (&bytes[2..], false)
    } else {
        (bytes, little_endian)
    };

    let (chunks, remainder) = bytes.as_chunks::<2>();
    let units = chunks.iter().map(|chunk| {
        if little_endian {
            u16::from_le_bytes(*chunk)
        } else {
            u16::from_be_bytes(*chunk)
        }
    });
    let mut text: String = char::decode_utf16(units)
        .map(|result| result.map(printable_char).unwrap_or('.'))
        .collect();
    if !remainder.is_empty() {
        text.push('.');
    }
    text
}

/// Payload filter mode. The first four modes mirror [`TextEncoding`]; Binary
/// accepts hex bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PayloadMode {
    #[default]
    Ascii,
    Utf8,
    Utf16Le,
    Utf16Be,
    Binary,
}

impl PayloadMode {
    pub const ALL: [Self; 5] = [
        Self::Ascii,
        Self::Utf8,
        Self::Utf16Le,
        Self::Utf16Be,
        Self::Binary,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Binary => "Binary",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Ascii => "text to find in the payload",
            Self::Utf8 => "UTF-8 text to find in the payload",
            Self::Utf16Le => "text encoded as UTF-16 little-endian",
            Self::Utf16Be => "text encoded as UTF-16 big-endian",
            Self::Binary => "hex bytes, e.g. 01 03 00 15",
        }
    }

    pub fn is_binary(self) -> bool {
        self == Self::Binary
    }

    pub fn encode(self, input: &str) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Ascii => {
                if !input.is_ascii() {
                    return Err("ASCII search only accepts ASCII characters".to_string());
                }
                Ok(input.as_bytes().to_vec())
            }
            Self::Utf8 => Ok(input.as_bytes().to_vec()),
            Self::Utf16Le => Ok(input
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()),
            Self::Utf16Be => Ok(input
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>()),
            Self::Binary => {
                let digits = input
                    .chars()
                    .filter(|character| character.is_ascii_hexdigit())
                    .count();
                if !digits.is_multiple_of(2) {
                    return Err("binary search needs an even number of hex digits".to_string());
                }
                hodeco_codec::parse_hex(input).map_err(|error| error.to_string())
            }
        }
    }
}

/// Normalizes user-entered hex into lowercase pairs separated by one space.
/// Non-hex characters are discarded, which makes binary search inputs and raw
/// payload fields self-correcting while typing or pasting.
pub fn normalize_hex_text(input: &str) -> String {
    let mut digits = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '0' && matches!(characters.peek(), Some('x' | 'X')) {
            characters.next();
            continue;
        }
        if character.is_ascii_hexdigit() {
            digits.push(character.to_ascii_lowercase());
        }
    }
    let mut output = String::with_capacity(digits.len() + digits.len() / 2);
    for (index, chunk) in digits.as_bytes().chunks(2).enumerate() {
        if index > 0 {
            output.push(' ');
        }
        output.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    output
}

pub const MESSAGE_STATES: [MessageState; 6] = [
    MessageState::Incoming,
    MessageState::Outgoing,
    MessageState::OutgoingPending,
    MessageState::OutgoingFailed,
    MessageState::OutgoingRejected,
    MessageState::OutgoingTimedOut,
];

pub fn message_state_label(state: MessageState) -> &'static str {
    match state {
        MessageState::Incoming => "Incoming",
        MessageState::Outgoing => "Outgoing",
        MessageState::OutgoingPending => "Pending",
        MessageState::OutgoingFailed => "Failed",
        MessageState::OutgoingRejected => "Rejected",
        MessageState::OutgoingTimedOut => "Timed out",
    }
}

pub fn service_ref(service: &ServiceInfo) -> ServiceRef {
    match &service.alias {
        Some(alias) => ServiceRef::Alias {
            alias: alias.clone(),
        },
        None => ServiceRef::Key {
            protocol: service.protocol.clone(),
            transport: service.transport.clone(),
            entry: service.entry.clone(),
        },
    }
}

/// Readable fallback used by tables and message rows.
pub fn service_ref_label(reference: &ServiceRef) -> String {
    match reference {
        ServiceRef::Alias { alias } => alias.clone(),
        ServiceRef::Key {
            protocol, entry, ..
        } => format!("{protocol}/{entry}"),
    }
}

/// Full identifier shown in details and destructive confirmations.
pub fn service_key_label(service: &ServiceInfo) -> String {
    format!(
        "{}/{}/{}",
        service.protocol, service.transport, service.entry
    )
}

pub fn composite_key(reference: &ServiceRef) -> String {
    match reference {
        ServiceRef::Alias { alias } => alias.clone(),
        ServiceRef::Key {
            protocol,
            transport,
            entry,
        } => format!("{protocol}/{transport}/{entry}"),
    }
}

pub fn refs_match(left: &ServiceRef, right: &ServiceRef) -> bool {
    match (left, right) {
        (ServiceRef::Alias { alias }, ServiceRef::Alias { alias: other }) => alias == other,
        (
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            },
            ServiceRef::Key {
                protocol: other_protocol,
                transport: other_transport,
                entry: other_entry,
            },
        ) => protocol == other_protocol && transport == other_transport && entry == other_entry,
        _ => false,
    }
}

pub fn find_service<'a>(
    services: &'a [ServiceInfo],
    reference: &ServiceRef,
) -> Option<&'a ServiceInfo> {
    match reference {
        ServiceRef::Alias { alias } => services
            .iter()
            .find(|service| service.alias.as_deref() == Some(alias.as_str())),
        ServiceRef::Key {
            protocol,
            transport,
            entry,
        } => services.iter().find(|service| {
            service.protocol == *protocol
                && service.transport == *transport
                && service.entry == *entry
        }),
    }
}

pub fn find_service_mut<'a>(
    services: &'a mut [ServiceInfo],
    reference: &ServiceRef,
) -> Option<&'a mut ServiceInfo> {
    match reference {
        ServiceRef::Alias { alias } => services
            .iter_mut()
            .find(|service| service.alias.as_deref() == Some(alias.as_str())),
        ServiceRef::Key {
            protocol,
            transport,
            entry,
        } => services.iter_mut().find(|service| {
            service.protocol == *protocol
                && service.transport == *transport
                && service.entry == *entry
        }),
    }
}

pub fn display_service(services: &[ServiceInfo], reference: &ServiceRef) -> String {
    if let ServiceRef::Key {
        protocol, entry, ..
    } = reference
    {
        if let Some(service) = find_service(services, reference)
            && let Some(alias) = &service.alias
        {
            return alias.clone();
        }
        format!("{protocol}/{entry}")
    } else {
        service_ref_label(reference)
    }
}

/// Protocols with an empty transport list expose no compatibility metadata and
/// are treated as unrestricted.
pub fn protocol_supports_transport(
    protocols: &[ProtocolInfo],
    protocol: &str,
    transport: &str,
) -> bool {
    protocols
        .iter()
        .find(|candidate| candidate.id == protocol)
        .is_none_or(|candidate| {
            candidate.transports.is_empty() || candidate.transports.iter().any(|id| id == transport)
        })
}

pub fn supported_transports<'a>(
    protocols: &[ProtocolInfo],
    protocol: &str,
    transports: &'a [TransportInfo],
) -> Vec<&'a TransportInfo> {
    transports
        .iter()
        .filter(|transport| protocol_supports_transport(protocols, protocol, &transport.id))
        .collect()
}

/// Resolves the protocol of a service reference without assuming a specific
/// protocol. Composite keys carry the protocol directly; aliases are resolved
/// against the current service list.
pub fn service_protocol<'a>(
    services: &'a [ServiceInfo],
    reference: &'a ServiceRef,
) -> Option<&'a str> {
    match reference {
        ServiceRef::Key { protocol, .. } => Some(protocol.as_str()),
        ServiceRef::Alias { .. } => {
            find_service(services, reference).map(|service| service.protocol.as_str())
        }
    }
}

pub fn message_protocol<'a>(services: &'a [ServiceInfo], message: &'a Message) -> Option<&'a str> {
    service_protocol(services, &message.service)
}

pub type MessageParser = fn(&[u8]) -> hodeco_codec::parsed::DecodeResult;

/// Finds the protocol-specific decoder for a protocol name.
pub fn parser_for_protocol(protocol: &str) -> Option<MessageParser> {
    match protocol {
        "zwave" => Some(zwave::decode),
        "zigbee" => Some(zigbee::decode),
        _ => None,
    }
}

pub fn message_parser(services: &[ServiceInfo], message: &Message) -> Option<MessageParser> {
    message_protocol(services, message).and_then(parser_for_protocol)
}

/// The browser-side Parsed view is available for protocols with a decoder.
pub fn message_has_parser(services: &[ServiceInfo], message: &Message) -> bool {
    message_parser(services, message).is_some()
}

/// Ordered catalog keys for a service: composite first, alias second. The
/// composite key is stable across alias renames; the alias is a readable
/// fallback.
pub fn entity_service_keys(services: &[ServiceInfo], reference: &ServiceRef) -> Vec<String> {
    let service = find_service(services, reference);
    let mut keys = Vec::new();

    let composite = match reference {
        ServiceRef::Key {
            protocol,
            transport,
            entry,
        } => Some(entities::composite_service_key(protocol, transport, entry)),
        ServiceRef::Alias { .. } => service.map(|service| {
            entities::composite_service_key(&service.protocol, &service.transport, &service.entry)
        }),
    };
    if let Some(key) = composite {
        keys.push(key);
    }

    let alias = match reference {
        ServiceRef::Alias { alias } => Some(alias.clone()),
        ServiceRef::Key { .. } => service.and_then(|service| service.alias.clone()),
    };
    if let Some(alias) = alias {
        keys.push(entities::alias_service_key(&alias));
    }

    keys
}

pub fn message_summary(services: &[ServiceInfo], message: &Message) -> String {
    let payload = message.payload.as_slice();
    if payload.is_empty() {
        return "Empty payload".to_string();
    }

    // Protocol-specific summaries are opt-in. Any other protocol (including
    // one the UI has never seen before) is displayed as raw bytes only.
    let Some(parser) = message_parser(services, message) else {
        return format!("Raw payload · {} bytes", payload.len());
    };

    let sample_len = payload.len().min(SUMMARY_DECODE_LIMIT);
    let decoded = parser(&payload[..sample_len]);
    let mut summary = if decoded.frames.is_empty() {
        "Raw payload".to_string()
    } else {
        decoded.summary
    };

    if summary.chars().count() > SUMMARY_MAX_CHARS {
        summary = summary.chars().take(SUMMARY_MAX_CHARS).collect();
        summary.push('…');
    }
    if sample_len < payload.len() {
        summary.push_str(&format!(" · {} bytes total", payload.len()));
    }
    summary
}

pub fn message_preview(message: &Message, limit: usize) -> String {
    hex_preview(message.payload.as_slice(), limit)
}

/// Formats raw bytes as lowercase-space hex, truncating beyond `limit` bytes.
pub fn hex_preview(bytes: &[u8], limit: usize) -> String {
    if bytes.len() <= limit {
        spaced_hex(bytes)
    } else {
        format!("{} …", spaced_hex(&bytes[..limit]))
    }
}

/// Pretty-prints a value as JSON with a safe fallback when serialization fails.
pub fn pretty_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|error| format!("{{unprintable: {error}}}"))
}

/// Converts the UI-side Z-Wave filter spec into protocol-neutral payload
/// expression leaves for the gateway.
pub fn to_payload_expr(
    spec: &hodeco_codec::filter::FilterSpec,
) -> Result<PayloadExpr, hodeco_codec::filter::FilterError> {
    let conditions = spec.conditions()?;
    Ok(PayloadExpr::or(
        conditions
            .into_iter()
            .map(|condition| {
                PayloadExpr::and(
                    condition
                        .leaves
                        .into_iter()
                        .map(|leaf| PayloadExpr::leaf(PayloadOffset::At(leaf.offset), leaf.bytes))
                        .collect(),
                )
            })
            .collect(),
    ))
}

/// ANDs non-empty expressions. An empty list has no predicate.
pub fn and_payloads(mut expressions: Vec<PayloadExpr>) -> Option<PayloadExpr> {
    match expressions.len() {
        0 => None,
        1 => expressions.pop(),
        _ => Some(PayloadExpr::and(expressions)),
    }
}

/// Parses a comma/space/semicolon separated node list. Empty means any node.
pub fn parse_node_list(text: &str) -> Result<Vec<u8>, String> {
    let mut nodes = Vec::new();
    for part in text.split([',', ' ', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some(node) = parse_node_id(part)? else {
            continue;
        };
        if !nodes.contains(&node) {
            nodes.push(node);
        }
    }
    Ok(nodes)
}

/// Parses the optional node box in a protocol filter. Empty means "any node".
pub fn parse_node_id(text: &str) -> Result<Option<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16)
    } else {
        text.parse::<u8>()
    };
    parsed
        .map(Some)
        .map_err(|_| format!("invalid node ID '{text}' (expected 0–255)"))
}

pub fn format_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M:%S").to_string()
}

pub fn format_datetime(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

pub fn format_uptime(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - started_at).num_seconds().max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

/// Formats a duration as milliseconds, seconds, `1m 20s`, or `2h 5m`.
pub fn format_duration(duration: chrono::TimeDelta) -> String {
    let seconds = duration.num_seconds().max(0);
    if seconds < 1 {
        format!("{} ms", duration.num_milliseconds().max(0))
    } else if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

pub fn short_id(id: uuid::Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::utils::Base64Bytes;
    use uuid::Uuid;

    fn service(alias: Option<&str>, entry: &str) -> ServiceInfo {
        ServiceInfo {
            alias: alias.map(str::to_string),
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: entry.to_string(),
            status: hodeco_contract::domain::ServiceStatus {
                state: hodeco_contract::domain::ServiceState::Running,
                error: None,
                detail: None,
            },
            params: Default::default(),
        }
    }

    fn message(payload: Vec<u8>) -> Message {
        Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(payload),
        }
    }

    #[test]
    fn payload_modes_encode_their_declared_encoding() {
        assert_eq!(PayloadMode::Ascii.encode("Hi").unwrap(), b"Hi");
        assert_eq!(PayloadMode::Utf8.encode("é").unwrap(), "é".as_bytes());
        assert_eq!(PayloadMode::Utf16Le.encode("A").unwrap(), vec![0x41, 0x00]);
        assert_eq!(PayloadMode::Utf16Be.encode("A").unwrap(), vec![0x00, 0x41]);
        assert_eq!(
            PayloadMode::Binary.encode("01 03 00 15").unwrap(),
            vec![1, 3, 0, 21]
        );
        assert!(PayloadMode::Ascii.encode("é").is_err());
        assert!(PayloadMode::Binary.encode("0").is_err());
    }

    #[test]
    fn hex_text_normalization_discards_noise_and_groups_pairs() {
        assert_eq!(normalize_hex_text("0x010A zz 0013"), "01 0a 00 13");
        assert_eq!(normalize_hex_text("1"), "1");
        assert_eq!(normalize_hex_text(""), "");
    }

    #[test]
    fn text_encodings_decode_hex_view_rows() {
        assert_eq!(TextEncoding::Ascii.decode(b"Hi\x00"), "Hi.");
        assert_eq!(TextEncoding::Utf8.decode("hé".as_bytes()), "hé");
        assert_eq!(
            TextEncoding::Utf16Le.decode(&[0x41, 0x00, 0x42, 0x00]),
            "AB"
        );
        assert_eq!(
            TextEncoding::Utf16Be.decode(&[0x00, 0x41, 0x00, 0x42]),
            "AB"
        );
    }

    fn decode_rows(encoding: TextEncoding, bytes: &[u8]) -> String {
        (0..bytes.len())
            .step_by(16)
            .map(|start| encoding.decode_row(bytes, start, (start + 16).min(bytes.len())))
            .collect()
    }

    #[test]
    fn row_decoding_keeps_multibyte_characters_intact() {
        let russian = "HTTP/1.1 200 OK\r\n\r\nПривет, hodeco!";
        assert!(decode_rows(TextEncoding::Utf8, russian.as_bytes()).contains("Привет, hodeco!"));

        let japanese = "こんにちは世界";
        assert_eq!(
            decode_rows(TextEncoding::Utf8, japanese.as_bytes()),
            japanese
        );

        let mut utf16le = vec![0xff, 0xfe];
        utf16le.extend(japanese.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_rows(TextEncoding::Utf16Le, &utf16le), japanese);

        let mut utf16be = vec![0xfe, 0xff];
        utf16be.extend(japanese.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_rows(TextEncoding::Utf16Be, &utf16be), japanese);
    }

    #[test]
    fn labels_fall_back_to_composite_keys() {
        let services = vec![service(None, "COM3")];
        let reference = service_ref(&services[0]);
        assert_eq!(service_ref_label(&reference), "zwave/COM3");
        assert_eq!(display_service(&services, &reference), "zwave/COM3");
        assert_eq!(service_key_label(&services[0]), "zwave/serial/COM3");
    }

    #[test]
    fn resolves_aliases_for_composite_message_refs() {
        let services = vec![service(Some("ZW"), "COM4")];
        let reference = ServiceRef::Key {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM4".to_string(),
        };
        assert_eq!(display_service(&services, &reference), "ZW");
    }

    #[test]
    fn previews_truncate_long_payloads() {
        let message = message((0..20).collect());
        assert!(message_preview(&message, 8).ends_with('…'));
        assert_eq!(message_preview(&message, 20).split(' ').count(), 20);
    }

    #[test]
    fn entity_keys_prefer_the_composite_key() {
        let services = vec![service(Some("ZW"), "COM4")];
        let expected = vec!["key:zwave/serial/COM4".to_string(), "alias:ZW".to_string()];
        assert_eq!(
            entity_service_keys(
                &services,
                &ServiceRef::Alias {
                    alias: "ZW".to_string(),
                },
            ),
            expected
        );
        assert_eq!(
            entity_service_keys(
                &services,
                &ServiceRef::Key {
                    protocol: "zwave".to_string(),
                    transport: "serial".to_string(),
                    entry: "COM4".to_string(),
                },
            ),
            expected
        );
    }

    #[test]
    fn parses_node_lists_for_the_pattern_helper() {
        assert_eq!(parse_node_list("2, 7; 9").unwrap(), vec![2, 7, 9]);
        assert_eq!(parse_node_list("").unwrap(), Vec::<u8>::new());
        assert!(parse_node_list("2, nope").is_err());
    }

    #[test]
    fn parses_optional_node_ids() {
        assert_eq!(parse_node_id("").unwrap(), None);
        assert_eq!(parse_node_id("5").unwrap(), Some(5));
        assert_eq!(parse_node_id("0x0A").unwrap(), Some(10));
        assert!(parse_node_id("300").is_err());
        assert!(parse_node_id("office").is_err());
    }

    #[test]
    fn large_messages_keep_summaries_bounded() {
        let services = vec![service(Some("ZW"), "COM4")];
        let message = message(vec![b'A'; 100_000]);
        let summary = message_summary(&services, &message);
        assert!(summary.contains("100000 bytes total"));
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS + 32);
    }

    #[test]
    fn non_zwave_messages_use_raw_summaries() {
        let services = vec![ServiceInfo {
            alias: Some("HTTP".to_string()),
            protocol: "http".to_string(),
            transport: "tcp".to_string(),
            entry: "api.example.test:443".to_string(),
            status: hodeco_contract::domain::ServiceStatus {
                state: hodeco_contract::domain::ServiceState::Running,
                error: None,
                detail: None,
            },
            params: Default::default(),
        }];
        let message = Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "HTTP".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x2d]),
        };
        assert_eq!(message_protocol(&services, &message), Some("http"));
        assert_eq!(
            message_summary(&services, &message),
            "Raw payload · 7 bytes"
        );

        let mut unresolved = services.clone();
        unresolved.clear();
        assert_eq!(message_protocol(&unresolved, &message), None);
        assert_eq!(
            message_summary(&unresolved, &message),
            "Raw payload · 7 bytes"
        );
    }

    #[test]
    fn zigbee_messages_use_the_zigbee_parser() {
        let services = vec![ServiceInfo {
            alias: Some("ZB".to_string()),
            protocol: "zigbee".to_string(),
            transport: "serial".to_string(),
            entry: "COM5".to_string(),
            status: hodeco_contract::domain::ServiceStatus {
                state: hodeco_contract::domain::ServiceState::Running,
                error: None,
                detail: Some("EZSP v4 · stack 0x5410".to_string()),
            },
            params: Default::default(),
        }];
        let version = Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "ZB".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![0x00, 0x80, 0x01, 0x00, 0x00, 0x04, 0x02, 0x10, 0x54]),
        };
        assert_eq!(message_protocol(&services, &version), Some("zigbee"));
        assert!(message_summary(&services, &version).contains("EZSP v4"));

        let ack = Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "ZB".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![0x81, 0x60, 0x59, 0x7e]),
        };
        assert_eq!(message_summary(&services, &ack), "ASH ACK · ack 1");
    }
}
