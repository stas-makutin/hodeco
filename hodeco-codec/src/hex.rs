//! Hex formatting and permissive hex input parsing.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpRow {
    /// Byte offset of the first byte on this row.
    pub offset: usize,
    /// Two-digit lowercase hex bytes separated by spaces.
    pub hex: String,
    /// Printable ASCII, with non-printable bytes shown as `.`.
    pub ascii: String,
}

impl DumpRow {
    pub fn offset_hex(&self) -> String {
        format!("{:08x}", self.offset)
    }
}

/// Formats bytes as a classic offset / hex / ASCII dump.
pub fn dump(bytes: &[u8], width: usize) -> Vec<DumpRow> {
    let width = width.max(1);
    bytes
        .chunks(width)
        .enumerate()
        .map(|(index, chunk)| DumpRow {
            offset: index * width,
            hex: spaced_hex(chunk),
            ascii: chunk
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect(),
        })
        .collect()
}

/// Formats bytes as lowercase hex separated by single spaces.
pub fn spaced_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(3));
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            output.push(' ');
        }
        push_hex_byte(&mut output, *byte);
    }
    output
}

/// Formats bytes as compact lowercase hex.
pub fn compact_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        push_hex_byte(&mut output, *byte);
    }
    output
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex_byte(output: &mut String, byte: u8) {
    output.push(HEX[(byte >> 4) as usize] as char);
    output.push(HEX[(byte & 0x0f) as usize] as char);
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HexError {
    #[error("invalid hex byte '{token}'")]
    InvalidByte { token: String },
    #[error("odd number of hex digits")]
    OddDigits,
}

/// Parses hex bytes from input that may use whitespace, commas, colons, or
/// compact pairs. `0x` prefixes and uppercase digits are accepted. Empty input
/// yields an empty payload.
pub fn parse_hex(input: &str) -> Result<Vec<u8>, HexError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let normalized = trimmed
        .replace("0x", " ")
        .replace("0X", " ")
        .replace([',', ':', ';', '-', '_', '|', '/'], " ");

    if let Some(character) = normalized
        .chars()
        .find(|character| !character.is_ascii_hexdigit() && !character.is_whitespace())
    {
        return Err(HexError::InvalidByte {
            token: character.to_string(),
        });
    }

    let tokens: Vec<&str> = normalized.split_whitespace().collect();

    // A compact string such as "010203" or "0x010203" arrives as one token.
    if tokens.len() == 1 && tokens[0].len() > 2 {
        let token = tokens[0];
        if !token.len().is_multiple_of(2) {
            return Err(HexError::OddDigits);
        }
        let mut output = Vec::with_capacity(token.len() / 2);
        for index in (0..token.len()).step_by(2) {
            output.push(parse_byte(&token[index..index + 2])?);
        }
        return Ok(output);
    }

    let mut output = Vec::with_capacity(tokens.len());
    for token in tokens {
        if token.len() > 2 {
            return Err(HexError::InvalidByte {
                token: token.to_string(),
            });
        }
        output.push(parse_byte(token)?);
    }
    Ok(output)
}

fn parse_byte(token: &str) -> Result<u8, HexError> {
    u8::from_str_radix(token, 16).map_err(|_| HexError::InvalidByte {
        token: token.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hex_styles() {
        assert_eq!(parse_hex("01 03 00 15 e9").unwrap(), [1, 3, 0, 21, 233]);
        assert_eq!(parse_hex("0x01, 0x03:15-E9").unwrap(), [1, 3, 21, 233]);
        assert_eq!(parse_hex("01030015e9").unwrap(), [1, 3, 0, 21, 233]);
        assert_eq!(parse_hex("1 3 0 f").unwrap(), [1, 3, 0, 15]);
        assert_eq!(parse_hex("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn rejects_invalid_hex() {
        assert!(matches!(parse_hex("zz"), Err(HexError::InvalidByte { .. })));
        assert_eq!(parse_hex("010"), Err(HexError::OddDigits));
        assert!(parse_hex("001").is_err());
    }

    #[test]
    fn dumps_offsets_and_ascii() {
        let rows = dump(b"Hello, Z-Wave!", 8);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset_hex(), "00000000");
        assert_eq!(rows[0].hex, "48 65 6c 6c 6f 2c 20 5a");
        assert_eq!(rows[0].ascii, "Hello, Z");
        assert_eq!(rows[1].offset, 8);
    }
}
