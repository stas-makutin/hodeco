//! Semantic decode result shared by the Parsed view and the message table.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Header,
    Length,
    FrameType,
    Command,
    Node,
    CommandClass,
    Value,
    Checksum,
    Control,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedField {
    pub label: String,
    pub value: String,
    pub kind: FieldKind,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Data,
    Ack,
    Nak,
    Can,
    Reset,
    ResetAck,
    Error,
    Unknown,
}

/// Routing and command metadata extracted from a Serial API frame.
///
/// The UI uses this for structured filtering so it does not have to infer
/// meaning from display labels. Control and unrecognised frames have an empty
/// address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameAddress {
    /// Nodes addressed by an outgoing `SendData`/`SendDataMulti` request.
    /// Broadcasts appear as node `0xFF`.
    pub targets: Vec<u8>,
    /// Node that originated an `ApplicationCommandHandler` frame.
    pub source: Option<u8>,
    /// Command class carried by the frame, when it can be determined.
    pub command_class: Option<u8>,
    /// Command within the command class, when it can be determined.
    pub command: Option<u8>,
}

impl FrameAddress {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
            && self.source.is_none()
            && self.command_class.is_none()
            && self.command.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub kind: FrameKind,
    pub range: Range<usize>,
    /// Short frame title, for example `ZW_Version · Response`.
    pub title: String,
    /// One-line description suitable for a table preview.
    pub summary: String,
    /// `None` for control/unrecognised frames.
    pub checksum_ok: Option<bool>,
    pub fields: Vec<DecodedField>,
    /// Routing/command metadata for filters and other semantic consumers.
    pub address: FrameAddress,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodeResult {
    pub summary: String,
    pub frames: Vec<DecodedFrame>,
    pub issues: Vec<String>,
}
