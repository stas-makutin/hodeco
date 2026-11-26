//! Z-Wave frame-layout helpers for the builder-style payload filter.
//!
//! The gateway only understands protocol-neutral payload expressions. This
//! module is the UI-side bridge: it turns explicit frame-type and field
//! selections into one or more AND groups of contiguous byte leaves. It
//! deliberately keeps all Z-Wave knowledge here so the gateway API stays
//! protocol-agnostic.
//!
//! A condition always targets one frame kind. Multiple frame kinds, or multiple
//! target nodes, become separate OR conditions. Contiguous included bytes are
//! coalesced into one leaf so omissions do not need wildcard padding. When a
//! variable-length field is followed by a matched field, the corresponding
//! length byte must also be matched; otherwise the later field would move
//! between frames. [`FilterSpec::conditions`] reports that case as
//! [`FilterError::DerivedLengthRequired`].

use crate::builder::{self, BuildError};
use crate::definitions::{self, FieldDef, FieldFormat, FieldRole, FieldType, LayoutField};
use crate::zwave::{
    FRAME_ACK, FRAME_CAN, FRAME_NAK, FRAME_REQUEST, FRAME_RESPONSE, FRAME_SOF,
    SERIAL_APPLICATION_COMMAND_HANDLER, SERIAL_GET_VERSION, SERIAL_SEND_DATA,
    SERIAL_SEND_DATA_MULTI,
};
use std::collections::BTreeMap;
use thiserror::Error;

/// A Serial API frame layout the filter can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FrameKind {
    SendData,
    SendDataMulti,
    Broadcast,
    GetVersion,
    SendDataCallback,
    ApplicationCommandHandler,
    Ack,
    Nak,
    Can,
}

impl FrameKind {
    pub const ALL: [Self; 9] = [
        Self::SendData,
        Self::SendDataMulti,
        Self::Broadcast,
        Self::GetVersion,
        Self::SendDataCallback,
        Self::ApplicationCommandHandler,
        Self::Ack,
        Self::Nak,
        Self::Can,
    ];

    /// Full label used by the filter dialog.
    pub fn label(self) -> String {
        let definitions = definitions::current();
        if let Some(id) = self.control_id() {
            let name = definitions
                .control_frame(id)
                .map(|frame| frame.name.clone())
                .unwrap_or_else(|| format!("0x{id:02x}"));
            return format!("{name} (0x{id:02x})");
        }
        let Some(id) = self.serial_command() else {
            return self.short_label();
        };
        let name = definitions
            .serial_command(id)
            .map(|command| command.name.as_str())
            .unwrap_or("Serial API");
        match self {
            Self::SendData | Self::SendDataMulti | Self::GetVersion => {
                format!("{name} request (0x{id:02x})")
            }
            Self::Broadcast => format!("Broadcast request ({name} 0xFF)"),
            Self::SendDataCallback => format!("{name} callback (0x{id:02x} response)"),
            Self::ApplicationCommandHandler => format!("{name} (0x{id:02x})"),
            Self::Ack | Self::Nak | Self::Can => unreachable!(),
        }
    }

    /// Compact label used by chips and condition previews.
    pub fn short_label(self) -> String {
        let definitions = definitions::current();
        if let Some(id) = self.control_id() {
            return definitions
                .control_frame(id)
                .map(|frame| frame.name.clone())
                .unwrap_or_else(|| format!("0x{id:02x}"));
        }
        let Some(id) = self.serial_command() else {
            return "Z-Wave".to_string();
        };
        let name = definitions
            .serial_command(id)
            .map(|command| command.name.clone())
            .unwrap_or_else(|| format!("0x{id:02x}"));
        match self {
            Self::Broadcast => "Broadcast".to_string(),
            Self::SendDataCallback => format!("{name} callback"),
            _ => name,
        }
    }

    pub fn direction_label(self) -> &'static str {
        match self {
            Self::SendData | Self::SendDataMulti | Self::Broadcast | Self::GetVersion => "Outgoing",
            Self::SendDataCallback | Self::ApplicationCommandHandler => "Incoming",
            Self::Ack | Self::Nak | Self::Can => "Control",
        }
    }

    pub fn frame_type(self) -> Option<u8> {
        match self {
            Self::SendData
            | Self::SendDataMulti
            | Self::Broadcast
            | Self::GetVersion
            | Self::ApplicationCommandHandler => Some(FRAME_REQUEST),
            Self::SendDataCallback => Some(FRAME_RESPONSE),
            Self::Ack | Self::Nak | Self::Can => None,
        }
    }

    pub fn serial_command(self) -> Option<u8> {
        match self {
            Self::SendData | Self::Broadcast | Self::SendDataCallback => Some(SERIAL_SEND_DATA),
            Self::SendDataMulti => Some(SERIAL_SEND_DATA_MULTI),
            Self::GetVersion => Some(SERIAL_GET_VERSION),
            Self::ApplicationCommandHandler => Some(SERIAL_APPLICATION_COMMAND_HANDLER),
            Self::Ack | Self::Nak | Self::Can => None,
        }
    }

    pub fn control_id(self) -> Option<u8> {
        match self {
            Self::Ack => Some(FRAME_ACK),
            Self::Nak => Some(FRAME_NAK),
            Self::Can => Some(FRAME_CAN),
            _ => None,
        }
    }

    pub fn uses_node(self) -> bool {
        matches!(
            self,
            Self::SendData
                | Self::SendDataMulti
                | Self::Broadcast
                | Self::ApplicationCommandHandler
        )
    }

    pub fn uses_command_data(self) -> bool {
        matches!(
            self,
            Self::SendData
                | Self::SendDataMulti
                | Self::Broadcast
                | Self::ApplicationCommandHandler
        )
    }

    pub fn uses_tx_options(self) -> bool {
        matches!(self, Self::SendData | Self::SendDataMulti | Self::Broadcast)
    }

    pub fn uses_callback_id(self) -> bool {
        matches!(
            self,
            Self::SendData | Self::SendDataMulti | Self::Broadcast | Self::SendDataCallback
        )
    }

    pub fn uses_rx_status(self) -> bool {
        self == Self::ApplicationCommandHandler
    }

    pub fn uses_tx_status(self) -> bool {
        self == Self::SendDataCallback
    }

    pub fn is_control(self) -> bool {
        matches!(self, Self::Ack | Self::Nak | Self::Can)
    }
}

/// Per-field "include in filter" switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Includes {
    pub sof: bool,
    pub frame_length: bool,
    pub frame_type: bool,
    pub serial_command: bool,
    pub node: bool,
    /// Node count, command data length, or ACH command length.
    pub derived_length: bool,
    pub command_class: bool,
    pub command: bool,
    pub command_data: bool,
    pub tx_options: bool,
    pub callback_id: bool,
    pub rx_status: bool,
    pub tx_status: bool,
    pub checksum: bool,
}

impl Default for Includes {
    fn default() -> Self {
        Self {
            sof: false,
            frame_length: false,
            frame_type: true,
            serial_command: true,
            node: false,
            derived_length: false,
            command_class: false,
            command: false,
            command_data: false,
            tx_options: false,
            callback_id: false,
            rx_status: false,
            tx_status: false,
            checksum: false,
        }
    }
}

impl Includes {
    /// Every byte of the frame is matched exactly.
    pub const fn exact() -> Self {
        Self {
            sof: true,
            frame_length: true,
            frame_type: true,
            serial_command: true,
            node: true,
            derived_length: true,
            command_class: true,
            command: true,
            command_data: true,
            tx_options: true,
            callback_id: true,
            rx_status: true,
            tx_status: true,
            checksum: true,
        }
    }
}

/// One typed command-class field selected in a filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandFieldMatch {
    pub enabled: bool,
    pub value: String,
}

/// A fixed-width command-class field and its offset in the command payload.
#[derive(Debug, Clone)]
pub struct CommandFieldSpan {
    pub field: FieldDef,
    pub offset: usize,
    pub width: usize,
}

/// The complete builder-style filter condition. Text inputs are parsed by the
/// UI before constructing this value. One spec targets one frame kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    pub frame: FrameKind,
    pub nodes: Vec<u8>,
    /// Editable node-list text; parsed into `nodes` by the UI.
    pub nodes_text: String,
    pub command_class: Option<u8>,
    pub command: Option<u8>,
    /// Bytes after the command-class and command bytes.
    pub command_data: Vec<u8>,
    /// Editable hex text; parsed into `command_data` by the UI.
    pub command_data_hex: String,
    /// Optional typed command-class fields. When any are enabled, the fixed
    /// prefix is built from these and `command_data_hex` becomes an extra tail.
    pub command_fields: BTreeMap<String, CommandFieldMatch>,
    pub tx_options: u8,
    pub callback_id: u8,
    pub rx_status: u8,
    pub tx_status: u8,
    pub includes: Includes,
}

impl Default for FilterSpec {
    fn default() -> Self {
        Self {
            frame: FrameKind::SendData,
            nodes: Vec::new(),
            nodes_text: String::new(),
            command_class: None,
            command: None,
            command_data: Vec::new(),
            command_data_hex: String::new(),
            command_fields: BTreeMap::new(),
            tx_options: 0,
            callback_id: 1,
            rx_status: 0,
            tx_status: 0,
            includes: Includes::default(),
        }
    }
}

/// One contiguous byte run inside a frame condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRun {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// One OR condition: a contiguous frame shape described by coalesced leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCondition {
    pub frame: FrameKind,
    /// Target or source node for node-specific conditions.
    pub node: Option<u8>,
    pub leaves: Vec<ByteRun>,
    /// Total length of the generated frame, including omitted bytes.
    pub frame_len: usize,
    /// Number of bytes covered by the leaves.
    pub matched_bytes: usize,
    /// Every byte of the generated frame is covered by a leaf.
    pub exact: bool,
}

impl FilterSpec {
    /// Fixed-width command-class fields that can be matched individually.
    pub fn command_field_spans(&self) -> Result<Vec<CommandFieldSpan>, FilterError> {
        let Some(class_id) = self.command_class else {
            return Ok(Vec::new());
        };
        let Some(command_id) = self.command else {
            return Ok(Vec::new());
        };
        let definitions = definitions::current();
        let Some(command) = definitions.command(class_id, command_id) else {
            // A class or command without a typed definition still supports raw
            // command bytes; do not turn that into a field error.
            return Ok(Vec::new());
        };

        let mut spans = Vec::new();
        let mut cursor = 0;
        for item in &command.fields {
            let LayoutField::Field(field) = item else {
                break;
            };
            if field.derived {
                if let Some(width) = field.length.or_else(|| field.kind.fixed_width()) {
                    cursor += width;
                    continue;
                }
                break;
            }
            if field.remaining || field.length_from.is_some() || field.count_from.is_some() {
                break;
            }
            let width = field
                .length
                .or_else(|| field.kind.fixed_width())
                .ok_or_else(|| {
                    FilterError::InvalidCommandField(format!("{} has no fixed width", field.label))
                })?;
            spans.push(CommandFieldSpan {
                field: field.as_ref().clone(),
                offset: cursor,
                width,
            });
            cursor += width;
        }
        Ok(spans)
    }

    /// Which command-data bytes participate in the condition. Typed fields are
    /// included independently, so disabling one field does not leave a zero
    /// byte in the match. Bytes after the last typed field are the raw tail.
    fn command_data_included(&self, len: usize) -> Vec<bool> {
        let typed = self.command_fields.values().any(|field| field.enabled);
        if !typed {
            return vec![self.includes.command_data; len];
        }

        let mut included = vec![false; len];
        let spans = match self.command_field_spans() {
            Ok(spans) => spans,
            Err(_) => return vec![self.includes.command_data; len],
        };
        let prefix_len = spans
            .last()
            .map(|span| span.offset + span.width)
            .unwrap_or(0)
            .min(len);
        for span in &spans {
            if self
                .command_fields
                .get(&span.field.name)
                .is_some_and(|field| field.enabled)
            {
                let end = (span.offset + span.width).min(len);
                included[span.offset.min(len)..end].fill(true);
            }
        }
        if self.includes.command_data {
            included[prefix_len..].fill(true);
        }
        included
    }

    /// Full command-class payload: class, command, then typed or raw data.
    pub fn command_payload(&self) -> Result<Vec<u8>, FilterError> {
        let class = self.command_class.unwrap_or(0);
        let command = self.command.unwrap_or(0);
        let typed = self.command_fields.values().any(|field| field.enabled);
        let raw = if self.command_data_hex.trim().is_empty() {
            self.command_data.clone()
        } else {
            crate::parse_hex(&self.command_data_hex).map_err(|_| FilterError::InvalidCommandData)?
        };
        if !typed {
            let mut payload = Vec::with_capacity(2 + raw.len());
            payload.push(class);
            payload.push(command);
            payload.extend_from_slice(&raw);
            return Ok(payload);
        }
        if self.command_class.is_none() || self.command.is_none() {
            return Err(FilterError::InvalidCommandField(
                "select a command class and command".to_string(),
            ));
        }
        let spans = self.command_field_spans()?;
        let prefix_len = spans
            .last()
            .map(|span| span.offset + span.width)
            .unwrap_or(0);
        let mut typed_payload = vec![0u8; prefix_len];
        for span in &spans {
            let Some(field_match) = self.command_fields.get(&span.field.name) else {
                continue;
            };
            if !field_match.enabled {
                continue;
            }
            let bytes = encode_command_field(&span.field, &field_match.value)
                .map_err(FilterError::InvalidCommandField)?;
            if bytes.len() != span.width {
                return Err(FilterError::InvalidCommandField(format!(
                    "{} must be {} byte(s), got {}",
                    span.field.label,
                    span.width,
                    bytes.len()
                )));
            }
            typed_payload[span.offset..span.offset + span.width].copy_from_slice(&bytes);
        }
        let mut payload = Vec::with_capacity(2 + typed_payload.len() + raw.len());
        payload.push(class);
        payload.push(command);
        payload.extend_from_slice(&typed_payload);
        payload.extend_from_slice(&raw);
        Ok(payload)
    }

    /// True when a variable-length field is followed by a matched field but the
    /// corresponding derived length is not included. The UI uses this to force
    /// the "Derived lengths" switch and to disable unchecking it.
    pub fn requires_derived_length(&self) -> bool {
        let uses_multi = self.frame == FrameKind::SendDataMulti;
        let uses_send = matches!(self.frame, FrameKind::SendData | FrameKind::Broadcast);
        let uses_ach = self.frame == FrameKind::ApplicationCommandHandler;

        // SendDataMulti has a variable-length node list before every command
        // field. The node count must be fixed for those offsets to be stable.
        if uses_multi
            && (self.includes.node
                || self.includes.command_class
                || self.includes.command
                || self.includes.command_data
                || self.includes.tx_options
                || self.includes.callback_id
                || self.includes.checksum)
        {
            return true;
        }

        // SendData, SendDataMulti and Broadcast put TX options and the callback
        // ID after the variable-length command data.
        if uses_send && (self.includes.tx_options || self.includes.callback_id) {
            return true;
        }

        // A checksum sits at the end of the frame. Its offset is stable when
        // the total frame length is matched, or when the variable part's length
        // is matched.
        if self.includes.checksum
            && !self.includes.frame_length
            && (uses_send || uses_multi || uses_ach)
        {
            return true;
        }

        false
    }

    /// Largest possible frame-length byte for the selected data. The builder
    /// performs the authoritative validation; this only keeps the layout from
    /// producing an invalid length byte before that check runs.
    fn build_reference(
        &self,
        frame: FrameKind,
        node: Option<u8>,
        nodes_override: Option<&[u8]>,
        payload: &[u8],
    ) -> Result<Vec<u8>, FilterError> {
        match frame {
            FrameKind::SendData => builder::build_send_data(
                node.unwrap_or(1),
                payload,
                self.tx_options,
                self.callback_id,
            ),
            FrameKind::SendDataMulti => {
                let nodes = nodes_override.map(Vec::from).unwrap_or_else(|| {
                    if self.nodes.is_empty() {
                        vec![1]
                    } else {
                        self.nodes.clone()
                    }
                });
                builder::build_send_data_multi(&nodes, payload, self.tx_options, self.callback_id)
            }
            FrameKind::Broadcast => builder::build_broadcast(payload, self.callback_id),
            FrameKind::GetVersion => Ok(builder::build_get_version()),
            FrameKind::SendDataCallback => {
                builder::build_response(SERIAL_SEND_DATA, &[self.callback_id, self.tx_status])
            }
            FrameKind::ApplicationCommandHandler => {
                let mut params = Vec::with_capacity(3 + payload.len());
                params.push(self.rx_status);
                params.push(node.unwrap_or(1));
                params.push(payload.len() as u8);
                params.extend_from_slice(payload);
                builder::build_request(SERIAL_APPLICATION_COMMAND_HANDLER, &params)
            }
            FrameKind::Ack | FrameKind::Nak | FrameKind::Can => {
                Ok(vec![frame.control_id().expect("control frame ID")])
            }
        }
        .map_err(FilterError::Build)
    }

    /// Generates OR conditions. Multiple frame kinds or target nodes are
    /// represented as separate conditions, never as wildcard masks.
    pub fn conditions(&self) -> Result<Vec<FrameCondition>, FilterError> {
        if self.requires_derived_length() && !self.includes.derived_length {
            return Err(FilterError::DerivedLengthRequired);
        }
        if self.includes.command_data
            && !self.command_data_hex.trim().is_empty()
            && crate::parse_hex(&self.command_data_hex).is_err()
        {
            return Err(FilterError::InvalidCommandData);
        }

        let mut conditions = Vec::new();
        for node in self.frame_nodes(self.frame) {
            self.push_layout(self.frame, node, None, &mut conditions)?;
        }
        if conditions.is_empty() {
            return Err(FilterError::NoFields);
        }
        Ok(conditions)
    }

    fn push_layout(
        &self,
        frame: FrameKind,
        node: Option<u8>,
        nodes_override: Option<&[u8]>,
        conditions: &mut Vec<FrameCondition>,
    ) -> Result<(), FilterError> {
        let payload = self.command_payload()?;
        let reference = self.build_reference(frame, node, nodes_override, &payload)?;
        let layout = self.layout(frame, node, nodes_override, &payload)?;
        if layout.bytes != reference {
            return Err(FilterError::InternalLayout);
        }
        if let Some(condition) = layout.into_condition(frame, node) {
            conditions.push(condition);
        }
        Ok(())
    }

    fn frame_nodes(&self, frame: FrameKind) -> Vec<Option<u8>> {
        // SendDataMulti matches its exact node set as one frame. A fixed offset
        // cannot express "any one of these nodes", so no single-node expansion
        // is generated.
        if frame == FrameKind::SendDataMulti || !frame.uses_node() || self.nodes.is_empty() {
            return vec![None];
        }
        self.nodes.iter().copied().map(Some).collect()
    }

    fn layout(
        &self,
        frame: FrameKind,
        node: Option<u8>,
        nodes_override: Option<&[u8]>,
        payload: &[u8],
    ) -> Result<Layout, FilterError> {
        let mut layout = Layout::default();
        if let Some(control) = frame.control_id() {
            layout.push(control, true);
            return Ok(layout);
        }

        layout.push(FRAME_SOF, self.includes.sof);
        layout.push(0, self.includes.frame_length);
        layout.push(
            frame.frame_type().unwrap_or(FRAME_REQUEST),
            self.includes.frame_type,
        );
        layout.push(
            frame.serial_command().unwrap_or(SERIAL_GET_VERSION),
            self.includes.serial_command,
        );
        self.push_definition_layout(&mut layout, frame, node, nodes_override, payload)?;
        layout.finish(self.includes.frame_length, self.includes.checksum)?;
        Ok(layout)
    }

    fn push_definition_layout(
        &self,
        layout: &mut Layout,
        frame: FrameKind,
        node: Option<u8>,
        nodes_override: Option<&[u8]>,
        payload: &[u8],
    ) -> Result<(), FilterError> {
        let definitions = definitions::current();
        let fields = frame_layout_fields(&definitions, frame).ok_or(FilterError::InternalLayout)?;
        let class = payload.first().copied().unwrap_or(0);
        let command = payload.get(1).copied().unwrap_or(0);
        let command_data = payload.get(2..).unwrap_or(&[]);
        let command_len = payload.len();
        let nodes = self.layout_nodes(nodes_override);
        let target = if frame == FrameKind::Broadcast {
            builder::BROADCAST_NODE_ID
        } else {
            node.unwrap_or(1)
        };
        let tx_options = if frame == FrameKind::Broadcast {
            builder::BROADCAST_TX_OPTIONS
        } else {
            self.tx_options
        };

        for field in fields.iter().filter_map(LayoutField::field) {
            match field.role {
                FieldRole::Length => {
                    let value = referenced_length(fields, &field.name, command_len, nodes.len());
                    layout.push(value, self.includes.derived_length);
                }
                FieldRole::Target => {
                    let include =
                        frame == FrameKind::Broadcast || (self.includes.node && node.is_some());
                    layout.push(target, include);
                }
                FieldRole::Targets => {
                    let include = self.includes.node && !self.nodes.is_empty();
                    for node in &nodes {
                        layout.push(*node, include);
                    }
                }
                FieldRole::Source => {
                    layout.push(node.unwrap_or(1), self.includes.node && node.is_some());
                }
                FieldRole::CommandClass => {
                    layout.push(
                        class,
                        self.includes.command_class && self.command_class.is_some(),
                    );
                    layout.push(command, self.includes.command && self.command.is_some());
                    for (byte, include) in command_data
                        .iter()
                        .zip(self.command_data_included(command_data.len()))
                    {
                        layout.push(*byte, include);
                    }
                }
                FieldRole::RxStatus => layout.push(self.rx_status, self.includes.rx_status),
                FieldRole::TxStatus => layout.push(self.tx_status, self.includes.tx_status),
                _ if field.kind == FieldType::TxOptions => {
                    layout.push(tx_options, self.includes.tx_options);
                }
                _ if field.kind == FieldType::CallbackId => {
                    layout.push(self.callback_id, self.includes.callback_id);
                }
                _ => {
                    let width = field
                        .length
                        .or_else(|| field.kind.fixed_width())
                        .unwrap_or(1);
                    for _ in 0..width {
                        layout.push(0, false);
                    }
                }
            }
        }
        Ok(())
    }

    fn layout_nodes(&self, nodes_override: Option<&[u8]>) -> Vec<u8> {
        nodes_override.map(Vec::from).unwrap_or_else(|| {
            if self.nodes.is_empty() {
                vec![1]
            } else {
                self.nodes.clone()
            }
        })
    }

    /// Human-readable condition list for a filter chip.
    pub fn summary(&self) -> String {
        let conditions = self.conditions().unwrap_or_default();
        let exact = !conditions.is_empty() && conditions.iter().all(|condition| condition.exact);
        format!(
            "Z-Wave: {}{}{}",
            self.frame.short_label(),
            if conditions.len() > 1 {
                format!(" ×{}", conditions.len())
            } else {
                String::new()
            },
            if exact { " (exact)" } else { "" }
        )
    }
}

/// Formats conditions for the dialog preview.
pub fn format_conditions(conditions: &[FrameCondition]) -> String {
    conditions
        .iter()
        .map(|condition| {
            let leaves = condition
                .leaves
                .iter()
                .map(|leaf| {
                    format!(
                        "@{}: {}",
                        leaf.offset,
                        leaf.bytes
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            format!(
                "{}{}  {}  ({}/{} bytes{})",
                condition.frame.short_label(),
                condition
                    .node
                    .map(|node| format!(" node {node}"))
                    .unwrap_or_default(),
                leaves,
                condition.matched_bytes,
                condition.frame_len,
                if condition.exact { ", exact" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FilterError {
    #[error("select a frame type")]
    NoFrames,
    #[error("select at least one field to match")]
    NoFields,
    #[error(
        "include the derived lengths (node count, data length, or command length) to match fields after variable-length data"
    )]
    DerivedLengthRequired,
    #[error("command data is not valid hex")]
    InvalidCommandData,
    #[error("{0}")]
    InvalidCommandField(String),
    #[error("{0}")]
    Build(#[from] BuildError),
    #[error("internal frame layout mismatch while building the filter preview")]
    InternalLayout,
}

fn parse_command_number(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(&hex.replace([' ', ',', ':'], ""), 16).ok()
    } else {
        text.replace([' ', ','], "").parse::<u64>().ok()
    }
}

fn parse_command_field_number(field: &FieldDef, text: &str) -> Option<u64> {
    if field.format == FieldFormat::Hex {
        let text = text
            .trim()
            .trim_start_matches("0x")
            .replace([' ', ',', ':', '-'], "");
        if text.is_empty() {
            return None;
        }
        u64::from_str_radix(&text, 16).ok()
    } else {
        parse_command_number(text)
    }
}

fn encode_command_field(field: &FieldDef, text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{} is empty", field.label));
    }
    let named = || {
        field
            .values
            .iter()
            .find(|value| value.name.eq_ignore_ascii_case(text))
            .map(|value| u64::from(value.value))
    };
    match field.kind {
        FieldType::Bool => Ok(vec![u8::from(matches!(
            text.to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes"
        ))]),
        FieldType::Enum
        | FieldType::Flags
        | FieldType::Uint8
        | FieldType::Duration
        | FieldType::TxOptions
        | FieldType::CallbackId
        | FieldType::Node => {
            let value = named()
                .or_else(|| parse_command_field_number(field, text))
                .filter(|value| *value <= u64::from(u8::MAX))
                .ok_or_else(|| format!("{} is not a valid 8-bit value", field.label))?;
            Ok(vec![value as u8])
        }
        FieldType::Uint16 => {
            let value = named()
                .or_else(|| parse_command_field_number(field, text))
                .filter(|value| *value <= u64::from(u16::MAX))
                .ok_or_else(|| format!("{} is not a valid 16-bit value", field.label))?;
            Ok(field.endian.encode_unsigned(value, 2))
        }
        FieldType::Uint32 => {
            let value = named()
                .or_else(|| parse_command_field_number(field, text))
                .filter(|value| *value <= u64::from(u32::MAX))
                .ok_or_else(|| format!("{} is not a valid 32-bit value", field.label))?;
            Ok(field.endian.encode_unsigned(value, 4))
        }
        FieldType::Int => {
            let value = text
                .replace(',', "")
                .parse::<i64>()
                .map_err(|_| format!("{} is not a signed integer", field.label))?;
            let width = field
                .length
                .or_else(|| field.kind.fixed_width())
                .ok_or_else(|| format!("{} has no fixed width", field.label))?;
            if !matches!(width, 1 | 2 | 4) {
                return Err(format!("{} has an unsupported width", field.label));
            }
            Ok(field.endian.encode_signed(value, width))
        }
        FieldType::Uint => {
            let value = parse_command_field_number(field, text)
                .ok_or_else(|| format!("{} is not an unsigned integer", field.label))?;
            let width = field
                .length
                .ok_or_else(|| format!("{} has no fixed width", field.label))?;
            Ok(field.endian.encode_unsigned(value, width))
        }
        FieldType::NodeList => {
            let mut nodes = Vec::new();
            for part in text.split([',', ';', ' ']).filter(|part| !part.is_empty()) {
                nodes.push(
                    part.parse::<u8>()
                        .map_err(|_| format!("{} has an invalid node ID", field.label))?,
                );
            }
            Ok(nodes)
        }
        FieldType::Hex | FieldType::CommandClassData => {
            crate::parse_hex(text).map_err(|error| format!("invalid {}: {error}", field.label))
        }
        FieldType::Text => Ok(text.as_bytes().to_vec()),
    }
}

fn frame_layout_fields(
    definitions: &definitions::ZWaveDefinitions,
    frame: FrameKind,
) -> Option<&[LayoutField]> {
    match frame {
        FrameKind::SendData | FrameKind::Broadcast => {
            Some(&definitions.serial_command(SERIAL_SEND_DATA)?.fields)
        }
        FrameKind::SendDataMulti => {
            Some(&definitions.serial_command(SERIAL_SEND_DATA_MULTI)?.fields)
        }
        FrameKind::GetVersion => Some(&definitions.serial_command(SERIAL_GET_VERSION)?.fields),
        FrameKind::SendDataCallback => definitions
            .serial_command(SERIAL_SEND_DATA)?
            .callback
            .as_ref()
            .map(|callback| callback.fields.as_slice()),
        FrameKind::ApplicationCommandHandler => Some(
            &definitions
                .serial_command(SERIAL_APPLICATION_COMMAND_HANDLER)?
                .fields,
        ),
        FrameKind::Ack | FrameKind::Nak | FrameKind::Can => None,
    }
}

fn referenced_length(
    fields: &[LayoutField],
    name: &str,
    command_len: usize,
    node_count: usize,
) -> u8 {
    for field in fields.iter().filter_map(LayoutField::field) {
        if field.length_from.as_deref() == Some(name) && field.role == FieldRole::CommandClass {
            return command_len.min(usize::from(u8::MAX)) as u8;
        }
        if field.count_from.as_deref() == Some(name) && field.kind == FieldType::NodeList {
            return node_count.min(usize::from(u8::MAX)) as u8;
        }
    }
    0
}

#[derive(Debug, Clone, Default)]
struct Layout {
    bytes: Vec<u8>,
    included: Vec<bool>,
}

impl Layout {
    fn push(&mut self, byte: u8, include: bool) {
        self.bytes.push(byte);
        self.included.push(include);
    }

    /// Fills in the frame-length byte and appends the checksum.
    fn finish(&mut self, include_length: bool, include_checksum: bool) -> Result<(), FilterError> {
        if self.bytes.len() < 3 {
            return Err(FilterError::InternalLayout);
        }
        let length = self.bytes.len() - 1;
        if length > usize::from(u8::MAX) {
            return Err(FilterError::Build(BuildError::FrameTooLong(length + 2)));
        }
        self.bytes[1] = length as u8;
        self.included[1] = include_length;
        let checksum = crate::zwave::checksum(&self.bytes[1..]);
        self.push(checksum, include_checksum);
        Ok(())
    }

    fn into_condition(self, frame: FrameKind, node: Option<u8>) -> Option<FrameCondition> {
        let frame_len = self.bytes.len();
        let matched_bytes = self.included.iter().filter(|included| **included).count();
        if matched_bytes == 0 {
            return None;
        }
        let mut leaves = Vec::new();
        let mut index = 0;
        while index < frame_len {
            if !self.included[index] {
                index += 1;
                continue;
            }
            let start = index;
            while index < frame_len && self.included[index] {
                index += 1;
            }
            leaves.push(ByteRun {
                offset: start as u32,
                bytes: self.bytes[start..index].to_vec(),
            });
        }
        Some(FrameCondition {
            frame,
            node,
            leaves,
            frame_len,
            matched_bytes,
            exact: matched_bytes == frame_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_data() -> FilterSpec {
        FilterSpec {
            frame: FrameKind::SendData,
            nodes: vec![2],
            command_class: Some(builder::CC_BASIC),
            command: Some(builder::BASIC_SET),
            command_data: vec![0x63],
            tx_options: 0,
            callback_id: 1,
            includes: Includes {
                node: true,
                command_class: true,
                command: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        }
    }

    fn bytes(condition: &FrameCondition) -> Vec<(u32, Vec<u8>)> {
        condition
            .leaves
            .iter()
            .map(|leaf| (leaf.offset, leaf.bytes.clone()))
            .collect()
    }

    #[test]
    fn filter_layouts_come_from_serial_command_roles() {
        let definitions = definitions::current();

        let send_data = frame_layout_fields(&definitions, FrameKind::SendData).unwrap();
        let send_fields: Vec<_> = send_data.iter().filter_map(LayoutField::field).collect();
        assert!(
            send_fields
                .iter()
                .any(|field| field.role == FieldRole::Target)
        );
        assert!(
            send_fields
                .iter()
                .any(|field| field.role == FieldRole::Length)
        );
        assert!(
            send_fields
                .iter()
                .any(|field| field.role == FieldRole::CommandClass)
        );
        assert!(
            send_fields
                .iter()
                .any(|field| field.kind == FieldType::TxOptions)
        );
        assert!(
            send_fields
                .iter()
                .any(|field| field.kind == FieldType::CallbackId)
        );

        let multi = frame_layout_fields(&definitions, FrameKind::SendDataMulti).unwrap();
        assert!(
            multi
                .iter()
                .filter_map(LayoutField::field)
                .any(|field| field.kind == FieldType::NodeList)
        );
        assert_eq!(referenced_length(multi, "nodeCount", 3, 2), 2);
        assert_eq!(referenced_length(multi, "dataLength", 3, 2), 3);

        let ach = frame_layout_fields(&definitions, FrameKind::ApplicationCommandHandler).unwrap();
        let ach_fields: Vec<_> = ach.iter().filter_map(LayoutField::field).collect();
        assert!(
            ach_fields
                .iter()
                .any(|field| field.role == FieldRole::RxStatus)
        );
        assert!(
            ach_fields
                .iter()
                .any(|field| field.role == FieldRole::CommandClass)
        );

        let callback = frame_layout_fields(&definitions, FrameKind::SendDataCallback).unwrap();
        let callback_fields: Vec<_> = callback.iter().filter_map(LayoutField::field).collect();
        assert!(
            callback_fields
                .iter()
                .any(|field| field.kind == FieldType::CallbackId)
        );
        assert!(
            callback_fields
                .iter()
                .any(|field| field.role == FieldRole::TxStatus)
        );
    }

    #[test]
    fn typed_command_fields_build_the_payload() {
        let mut spec = FilterSpec {
            frame: FrameKind::SendData,
            nodes: vec![2],
            command_class: Some(builder::CC_BASIC),
            command: Some(builder::BASIC_SET),
            includes: Includes {
                node: true,
                command_class: true,
                command: true,
                command_data: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        spec.command_fields.insert(
            "value".to_string(),
            CommandFieldMatch {
                enabled: true,
                value: "99".to_string(),
            },
        );

        let spans = spec.command_field_spans().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].field.name, "value");
        assert_eq!(spans[0].offset, 0);
        assert_eq!(spans[0].width, 1);
        assert_eq!(spec.command_payload().unwrap(), vec![0x20, 0x01, 0x63]);

        spec.command_data_hex = "aa".to_string();
        assert_eq!(
            spec.command_payload().unwrap(),
            vec![0x20, 0x01, 0x63, 0xaa]
        );

        let condition = spec.conditions().unwrap().remove(0);
        assert!(condition.leaves.iter().any(|leaf| leaf.bytes.ends_with(&[
            builder::CC_BASIC,
            builder::BASIC_SET,
            0x63,
            0xaa
        ])));
    }

    #[test]
    fn disabled_command_fields_are_not_matched() {
        let mut spec = FilterSpec {
            frame: FrameKind::SendData,
            nodes: vec![2],
            command_class: Some(0x70),
            command: Some(0x04),
            includes: Includes {
                node: true,
                command_class: true,
                command: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        spec.command_fields.insert(
            "parameterNumber".to_string(),
            CommandFieldMatch {
                enabled: true,
                value: "5".to_string(),
            },
        );
        spec.command_fields.insert(
            "size".to_string(),
            CommandFieldMatch {
                enabled: false,
                value: "1".to_string(),
            },
        );

        assert_eq!(
            spec.command_payload().unwrap(),
            vec![0x70, 0x04, 0x00, 0x05, 0x00]
        );
        let condition = spec.conditions().unwrap().remove(0);
        assert!(
            condition
                .leaves
                .iter()
                .any(|leaf| leaf.bytes.ends_with(&[0x70, 0x04, 0x00, 0x05]))
        );
        assert!(
            !condition
                .leaves
                .iter()
                .any(|leaf| leaf.bytes.ends_with(&[0x70, 0x04, 0x00, 0x05, 0x00]))
        );
    }

    #[test]
    fn typed_command_fields_stop_at_variable_width() {
        let spec = FilterSpec {
            command_class: Some(0x70),
            command: Some(0x04),
            ..FilterSpec::default()
        };
        let spans = spec.command_field_spans().unwrap();
        assert_eq!(
            spans
                .iter()
                .map(|span| span.field.name.as_str())
                .collect::<Vec<_>>(),
            vec!["parameterNumber", "size"]
        );
    }

    #[test]
    fn raw_command_bytes_work_for_classes_without_definitions() {
        let spec = FilterSpec {
            command_class: Some(0x2A),
            command: Some(0x01),
            command_data: vec![0x1e],
            ..FilterSpec::default()
        };
        assert!(spec.command_field_spans().unwrap().is_empty());
        assert_eq!(spec.command_payload().unwrap(), vec![0x2A, 0x01, 0x1e]);
    }

    #[test]
    fn typed_command_field_rejects_invalid_value() {
        let mut spec = FilterSpec {
            command_class: Some(builder::CC_BASIC),
            command: Some(builder::BASIC_SET),
            ..FilterSpec::default()
        };
        spec.command_fields.insert(
            "value".to_string(),
            CommandFieldMatch {
                enabled: true,
                value: "not-a-number".to_string(),
            },
        );
        assert!(matches!(
            spec.command_payload(),
            Err(FilterError::InvalidCommandField(_))
        ));
    }

    #[test]
    fn frame_type_and_serial_command_are_one_default_leaf() {
        let spec = FilterSpec {
            includes: Includes::default(),
            ..FilterSpec::default()
        };
        let conditions = spec.conditions().unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(
            bytes(&conditions[0]),
            vec![(2, vec![FRAME_REQUEST, SERIAL_SEND_DATA])]
        );
        assert!(!conditions[0].exact);
    }

    #[test]
    fn send_data_condition_cannot_match_the_callback_layout() {
        let spec = send_data();
        let conditions = spec.conditions().unwrap();
        let condition = &conditions[0];
        assert_eq!(
            bytes(condition),
            vec![
                (2, vec![FRAME_REQUEST, SERIAL_SEND_DATA, 2]),
                (6, vec![0x20, builder::BASIC_SET]),
            ]
        );

        let callback = builder::build_response(SERIAL_SEND_DATA, &[2, 0]).unwrap();
        assert_eq!(callback[2], FRAME_RESPONSE);
        assert_ne!(callback[2], condition.leaves[0].bytes[0]);
    }

    #[test]
    fn omitted_bytes_are_not_part_of_the_expression() {
        let condition = send_data().conditions().unwrap().remove(0);
        assert!(!condition.leaves.iter().any(|leaf| leaf.offset == 5));
        assert!(!condition.leaves.iter().any(|leaf| leaf.offset == 9));
        assert_eq!(condition.matched_bytes, 5);
        assert_eq!(condition.frame_len, 12);
    }

    #[test]
    fn exact_send_data_matches_the_builder_frame_as_one_leaf() {
        let spec = FilterSpec {
            includes: Includes::exact(),
            ..send_data()
        };
        let condition = spec.conditions().unwrap().remove(0);
        let expected = builder::build_send_data(2, &[0x20, 0x01, 0x63], 0, 1).unwrap();
        assert!(condition.exact);
        assert_eq!(condition.leaves.len(), 1);
        assert_eq!(condition.leaves[0].offset, 0);
        assert_eq!(condition.leaves[0].bytes, expected);
        assert_eq!(condition.matched_bytes, expected.len());
        assert_eq!(condition.frame_len, expected.len());
    }

    #[test]
    fn ach_condition_coalesces_the_command_fields() {
        let spec = FilterSpec {
            frame: FrameKind::ApplicationCommandHandler,
            nodes: vec![9],
            command_class: Some(builder::CC_BASIC),
            command: Some(builder::BASIC_REPORT),
            command_data: vec![0x63],
            includes: Includes {
                node: true,
                command_class: true,
                command: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        let condition = spec.conditions().unwrap().remove(0);
        assert_eq!(
            bytes(&condition),
            vec![
                (2, vec![FRAME_REQUEST, SERIAL_APPLICATION_COMMAND_HANDLER]),
                (5, vec![9]),
                (7, vec![0x20, builder::BASIC_REPORT]),
            ]
        );
    }

    #[test]
    fn multiple_nodes_generate_independent_or_conditions() {
        let spec = FilterSpec {
            nodes: vec![2, 7],
            ..send_data()
        };
        let conditions = spec.conditions().unwrap();
        assert_eq!(conditions.len(), 2);
        assert_eq!(conditions[0].node, Some(2));
        assert_eq!(conditions[1].node, Some(7));
        assert_eq!(conditions[0].leaves[0].bytes.last(), Some(&2));
        assert_eq!(conditions[1].leaves[0].bytes.last(), Some(&7));
    }

    #[test]
    fn multiple_frames_generate_separate_conditions() {
        let spec = FilterSpec {
            frame: FrameKind::ApplicationCommandHandler,
            ..FilterSpec::default()
        };
        let first = spec.conditions().unwrap();
        let second = FilterSpec {
            frame: FrameKind::SendDataCallback,
            ..FilterSpec::default()
        }
        .conditions()
        .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].frame, FrameKind::ApplicationCommandHandler);
        assert_eq!(second[0].frame, FrameKind::SendDataCallback);
    }

    #[test]
    fn send_data_multi_uses_only_the_entire_node_set() {
        let spec = FilterSpec {
            frame: FrameKind::SendDataMulti,
            nodes: vec![2, 7],
            includes: Includes {
                node: true,
                derived_length: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        let conditions = spec.conditions().unwrap();
        assert_eq!(conditions.len(), 1);
        let leaves = bytes(&conditions[0]);
        assert_eq!(
            leaves[0],
            (2, vec![FRAME_REQUEST, SERIAL_SEND_DATA_MULTI, 2, 2, 7, 2])
        );
        assert!(!leaves.iter().any(|(_, bytes)| bytes == &[1, 2]));
        assert!(!leaves.iter().any(|(_, bytes)| bytes == &[1, 7]));
    }

    #[test]
    fn derived_length_is_required_before_tx_options() {
        let mut spec = send_data();
        spec.includes.tx_options = true;
        assert!(spec.requires_derived_length());
        assert_eq!(spec.conditions(), Err(FilterError::DerivedLengthRequired));

        spec.includes.derived_length = true;
        let condition = spec.conditions().unwrap().remove(0);
        assert_eq!(
            bytes(&condition),
            vec![
                (
                    2,
                    vec![
                        FRAME_REQUEST,
                        SERIAL_SEND_DATA,
                        2,
                        3,
                        0x20,
                        builder::BASIC_SET,
                    ],
                ),
                (9, vec![0]),
            ]
        );
    }

    #[test]
    fn send_data_multi_requires_the_node_count_before_command_fields() {
        let spec = FilterSpec {
            frame: FrameKind::SendDataMulti,
            includes: Includes {
                command_class: true,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        assert!(spec.requires_derived_length());
        assert_eq!(spec.conditions(), Err(FilterError::DerivedLengthRequired));
    }

    #[test]
    fn checksum_without_length_is_rejected() {
        let mut spec = send_data();
        spec.includes.checksum = true;
        assert!(spec.requires_derived_length());
        assert_eq!(spec.conditions(), Err(FilterError::DerivedLengthRequired));

        spec.includes.frame_length = true;
        assert!(!spec.requires_derived_length());
        let condition = spec.conditions().unwrap().remove(0);
        assert_eq!(condition.leaves.last().unwrap().offset, 11);
    }

    #[test]
    fn control_frames_are_single_exact_conditions() {
        let specs = [
            (FrameKind::Ack, FRAME_ACK),
            (FrameKind::Nak, FRAME_NAK),
            (FrameKind::Can, FRAME_CAN),
        ];
        for (frame, byte) in specs {
            let spec = FilterSpec {
                frame,
                ..FilterSpec::default()
            };
            let condition = spec.conditions().unwrap().remove(0);
            assert!(condition.exact);
            assert_eq!(condition.leaves[0].offset, 0);
            assert_eq!(condition.leaves[0].bytes, vec![byte]);
        }
    }

    #[test]
    fn broadcast_always_matches_node_ff() {
        let spec = FilterSpec {
            frame: FrameKind::Broadcast,
            includes: Includes {
                node: false,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        let condition = spec.conditions().unwrap().remove(0);
        assert_eq!(
            condition.leaves[0].bytes,
            vec![FRAME_REQUEST, SERIAL_SEND_DATA, 0xff]
        );
    }

    #[test]
    fn no_fields_is_an_error() {
        let spec = FilterSpec {
            includes: Includes {
                frame_type: false,
                serial_command: false,
                ..Includes::default()
            },
            ..FilterSpec::default()
        };
        assert_eq!(spec.conditions(), Err(FilterError::NoFields));
    }

    #[test]
    fn summary_reports_exact_frames() {
        let spec = FilterSpec {
            includes: Includes::exact(),
            ..send_data()
        };
        assert_eq!(spec.summary(), "Z-Wave: ZW_SendData (exact)");
        assert_eq!(send_data().summary(), "Z-Wave: ZW_SendData");
    }

    #[test]
    fn invalid_command_data_text_is_rejected() {
        let mut spec = send_data();
        spec.includes.command_data = true;
        spec.command_data_hex = "123".to_string();
        assert_eq!(spec.conditions(), Err(FilterError::InvalidCommandData));
        spec.command_data_hex = "63".to_string();
        assert!(spec.conditions().is_ok());
    }

    #[test]
    fn preview_formats_leaves_without_wildcards() {
        let preview = format_conditions(&send_data().conditions().unwrap());
        assert!(preview.contains("@2: 00 13 02"));
        assert!(preview.contains("@6: 20 01"));
        assert!(!preview.contains("??"));
    }
}
