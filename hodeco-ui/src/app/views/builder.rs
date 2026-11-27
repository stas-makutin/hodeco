//! Z-Wave message builder dialog.
//!
//! The dialog is a thin presentation layer over `hodeco-codec::builder`; all
//! byte-level validation happens in the codec crate.

use super::super::App;
use crate::model;
use crate::ui::{params, widgets};
use egui::{ComboBox, Context, Id, RichText, TextEdit, Ui};
use hodeco_codec::builder::{self as codec, SpecialOperation};
use hodeco_codec::definitions::{
    self, Endian, FieldDef, FieldFormat, FieldRole, FieldType, LayoutField, PackedGroupDef,
    SerialBuilderKind, SerialCommandDef,
};
use hodeco_codec::entities;
use hodeco_codec::hex::spaced_hex;
use hodeco_codec::zigbee as zigbee_codec;
use hodeco_contract::catalog::SERVICE_PARAM_OUTGOING_MAX_TTL;
use hodeco_contract::domain::{ProtocolInfo, ServiceInfo, ServiceRef};
use hodeco_contract::utils::{Base64Bytes, HumanDuration};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderOperation {
    /// Universal escape hatch available for every protocol.
    RawMessage,
    /// Z-Wave-only host-side operations provided by `hodeco-codec`.
    Special(SpecialOperation),
    /// A Serial API request generated from a `zwave.json` serial command.
    SerialCommand(u8),
    /// An EZSP request generated from a `zigbee.json` command.
    EzspCommand(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    Generic,
    SendData,
    SendDataMulti,
    Broadcast,
    RawFrame,
    RawMessage,
}

impl BuilderOperation {
    fn default_for_protocol(protocol: &str) -> Self {
        default_operation_for(protocol)
    }

    fn kind(self) -> OperationKind {
        match self {
            Self::RawMessage => OperationKind::RawMessage,
            Self::Special(SpecialOperation::RawFrame) => OperationKind::RawFrame,
            Self::Special(SpecialOperation::Broadcast) => OperationKind::Broadcast,
            Self::SerialCommand(id) => definitions::current()
                .serial_command(id)
                .map(|command| match command.builder {
                    SerialBuilderKind::Generic => OperationKind::Generic,
                    SerialBuilderKind::SendData => OperationKind::SendData,
                    SerialBuilderKind::SendDataMulti => OperationKind::SendDataMulti,
                })
                .unwrap_or(OperationKind::Generic),
            Self::EzspCommand(_) => OperationKind::Generic,
        }
    }

    fn label(self) -> String {
        match self {
            Self::RawMessage => "Raw message".to_string(),
            Self::Special(operation) => operation.label().to_string(),
            Self::SerialCommand(id) => definitions::current()
                .serial_command(id)
                .map(|command| format!("{} (0x{:02x})", command.name, command.id))
                .unwrap_or_else(|| format!("Serial API command 0x{id:02x}")),
            Self::EzspCommand(id) => zigbee_codec::current()
                .command(id)
                .map(|command| format!("{} (0x{id:04x})", command.name))
                .unwrap_or_else(|| format!("EZSP command 0x{id:04x}")),
        }
    }

    fn priority(self) -> i32 {
        match self {
            Self::RawMessage => 0,
            Self::Special(operation) => operation.priority(),
            Self::SerialCommand(id) => definitions::current()
                .serial_command(id)
                .map(|command| command.priority)
                .unwrap_or(1000),
            Self::EzspCommand(id) => zigbee_codec::current()
                .command(id)
                .map(|command| command.priority)
                .unwrap_or(1000),
        }
    }

    fn needs_nodes(self) -> bool {
        matches!(
            self.kind(),
            OperationKind::SendData | OperationKind::SendDataMulti
        )
    }

    fn needs_command(self) -> bool {
        matches!(
            self.kind(),
            OperationKind::SendData | OperationKind::SendDataMulti | OperationKind::Broadcast
        )
    }

    fn uses_tx_options(self) -> bool {
        matches!(
            self.kind(),
            OperationKind::SendData | OperationKind::SendDataMulti
        )
    }

    fn serial_command_id(self) -> Option<u8> {
        match self {
            Self::SerialCommand(id) => Some(id),
            _ => None,
        }
    }

    fn uses_callback(self) -> bool {
        matches!(
            self.kind(),
            OperationKind::SendData | OperationKind::SendDataMulti | OperationKind::Broadcast
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandPreset {
    BasicSet,
    BasicGet,
    BasicReport,
    SwitchBinarySet,
    SwitchBinaryGet,
    SwitchBinaryReport,
    SwitchMultilevelSet,
    SwitchMultilevelGet,
    SwitchMultilevelReport,
    MeterGet,
    MeterReport,
    SceneActivationSet,
    Generic,
}

impl CommandPreset {
    fn from_ids(class_id: u8, command_id: u8) -> Self {
        match (class_id, command_id) {
            (0x20, 0x01) => Self::BasicSet,
            (0x20, 0x02) => Self::BasicGet,
            (0x20, 0x03) => Self::BasicReport,
            (0x25, 0x01) => Self::SwitchBinarySet,
            (0x25, 0x02) => Self::SwitchBinaryGet,
            (0x25, 0x03) => Self::SwitchBinaryReport,
            (0x26, 0x01) => Self::SwitchMultilevelSet,
            (0x26, 0x02) => Self::SwitchMultilevelGet,
            (0x26, 0x03) => Self::SwitchMultilevelReport,
            (0x32, 0x01) => Self::MeterGet,
            (0x32, 0x02) => Self::MeterReport,
            (0x2B, 0x01) => Self::SceneActivationSet,
            _ => Self::Generic,
        }
    }

    fn uses_value(self) -> bool {
        matches!(
            self,
            Self::BasicSet
                | Self::BasicReport
                | Self::SwitchBinarySet
                | Self::SwitchBinaryReport
                | Self::SwitchMultilevelSet
                | Self::SwitchMultilevelReport
        )
    }

    fn uses_bool_value(self) -> bool {
        matches!(self, Self::SwitchBinarySet | Self::SwitchBinaryReport)
    }

    fn uses_scene(self) -> bool {
        self == Self::SceneActivationSet
    }

    fn uses_meter(self) -> bool {
        self == Self::MeterReport
    }
}

#[derive(Debug, Clone)]
pub enum SerialFieldState {
    Text(String),
    Bool(bool),
    Enum(u8),
    Flags(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuilderPurpose {
    Send,
    TaskMessage { action_index: usize },
}

enum BuilderOutcome {
    Send(Vec<u8>),
    UseMessage(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct BuilderDialog {
    pub purpose: BuilderPurpose,
    pub service: Option<ServiceRef>,
    pub protocol: String,
    pub operation: BuilderOperation,
    /// `None` selects the raw command-class byte editor.
    pub command_class: Option<u8>,
    pub command_id: Option<u8>,
    pub callback_id: String,
    pub node: String,
    pub nodes: String,
    pub value: String,
    pub scene_id: String,
    pub dimming: String,
    pub meter_type: String,
    pub meter_value: String,
    pub raw_cc: String,
    pub raw_serial: String,
    pub raw_payload: String,
    pub serial_fields: BTreeMap<String, SerialFieldState>,
    pub serial_params: String,
    pub command_params: String,
    /// Command ID and parameters for a class with no named command definition.
    pub raw_command: String,
    pub zigbee_sequence: String,
    /// Selected ZCL cluster for EZSP commands that carry a ZCL frame.
    /// `None` leaves the raw `messageContents` field editable.
    pub zcl_cluster: Option<u16>,
    pub zcl_general: bool,
    pub zcl_command: Option<u8>,
    pub zcl_sequence: String,
    pub zcl_direction_to_client: bool,
    pub zcl_disable_default_response: bool,
    pub zcl_manufacturer: String,
    pub zcl_fields: BTreeMap<String, SerialFieldState>,
    pub zcl_raw: String,
    pub ttl: String,
    pub error: Option<String>,
}

pub fn new_builder(
    service: Option<ServiceRef>,
    services: &[ServiceInfo],
    protocols: &[ProtocolInfo],
) -> BuilderDialog {
    let selected_service = service.or_else(|| services.first().map(model::service_ref));
    let protocol = selected_service
        .as_ref()
        .and_then(|reference| model::find_service(services, reference))
        .map(|service| service.protocol.clone())
        .or_else(|| protocols.first().map(|protocol| protocol.id.clone()))
        .unwrap_or_else(|| "zwave".to_string());
    BuilderDialog {
        purpose: BuilderPurpose::Send,
        service: selected_service,
        protocol: protocol.clone(),
        operation: BuilderOperation::default_for_protocol(&protocol),
        command_class: Some(0x25),
        command_id: Some(0x01),
        callback_id: "1".to_string(),
        node: String::new(),
        nodes: String::new(),
        value: "255".to_string(),
        scene_id: "1".to_string(),
        dimming: "0".to_string(),
        meter_type: "0x01".to_string(),
        meter_value: String::new(),
        raw_cc: String::new(),
        raw_serial: "01 03 00 15".to_string(),
        raw_payload: String::new(),
        serial_fields: BTreeMap::new(),
        serial_params: String::new(),
        command_params: String::new(),
        raw_command: String::new(),
        zigbee_sequence: "0".to_string(),
        zcl_cluster: None,
        zcl_general: false,
        zcl_command: None,
        zcl_sequence: "0".to_string(),
        zcl_direction_to_client: false,
        zcl_disable_default_response: false,
        zcl_manufacturer: String::new(),
        zcl_fields: BTreeMap::new(),
        zcl_raw: String::new(),
        ttl: String::new(),
        error: None,
    }
}

pub fn new_task_message_builder(
    service: Option<ServiceRef>,
    services: &[ServiceInfo],
    protocols: &[ProtocolInfo],
    action_index: usize,
    current: &Base64Bytes,
    ttl: Option<HumanDuration>,
) -> BuilderDialog {
    let service = service.filter(
        |reference| !matches!(reference, ServiceRef::Alias { alias } if alias.trim().is_empty()),
    );
    let mut dialog = new_builder(service, services, protocols);
    dialog.purpose = BuilderPurpose::TaskMessage { action_index };
    dialog.ttl = ttl.map(|ttl| ttl.to_string()).unwrap_or_default();
    let bytes = current.as_slice();
    if !bytes.is_empty() && !restore_protocol_builder(&mut dialog, bytes) {
        dialog.operation = BuilderOperation::RawMessage;
        dialog.raw_payload = spaced_hex(bytes);
    }
    dialog
}

/// Reopens a composed payload in its protocol-specific builder when a parser
/// exists, otherwise the caller falls back to raw message mode.
fn restore_protocol_builder(dialog: &mut BuilderDialog, payload: &[u8]) -> bool {
    match dialog.protocol.as_str() {
        "zwave" => restore_zwave_builder(dialog, payload),
        "zigbee" => restore_zigbee_builder(dialog, payload),
        _ => false,
    }
}

fn restore_zigbee_builder(dialog: &mut BuilderDialog, payload: &[u8]) -> bool {
    if payload.len() < zigbee_codec::EZSP_HEADER_LEN
        || payload[1] & 0x80 != 0
        || payload[2] & 0x03 != 1
    {
        return false;
    }
    let command_id = u16::from_le_bytes([payload[3], payload[4]]);
    let definitions = zigbee_codec::current();
    let Some(command) = definitions.command(command_id) else {
        return false;
    };
    let params = &payload[zigbee_codec::EZSP_HEADER_LEN..];
    let Some(field_bytes) = zigbee_codec::parameter_field_bytes(command_id, params) else {
        return false;
    };
    dialog.operation = BuilderOperation::EzspCommand(command_id);
    dialog.zigbee_sequence = payload[0].to_string();
    dialog.serial_params.clear();
    dialog.reset_zcl();
    dialog.serial_fields.clear();
    if command.fields.is_empty() && !command.no_request_parameters {
        dialog.serial_params = spaced_hex(params);
    } else {
        for field in command.fields.iter().flat_map(LayoutField::fields) {
            if field.derived {
                continue;
            }
            if let Some(bytes) = field_bytes.get(&field.name)
                && let Some(state) = ezsp_state_from_bytes(field, bytes)
            {
                dialog.serial_fields.insert(field.name.clone(), state);
            }
        }
    }
    dialog.build().ok().as_deref() == Some(payload)
}

fn ezsp_state_from_bytes(field: &FieldDef, bytes: &[u8]) -> Option<SerialFieldState> {
    match field.kind {
        FieldType::Bool => Some(SerialFieldState::Bool(
            bytes.first().is_some_and(|byte| *byte != 0),
        )),
        FieldType::Enum => Some(SerialFieldState::Enum(*bytes.first()?)),
        FieldType::Flags | FieldType::TxOptions => Some(SerialFieldState::Flags(
            u8::try_from(field.endian.read_unsigned(bytes)).ok()?,
        )),
        FieldType::Node => Some(SerialFieldState::Text(bytes.first()?.to_string())),
        FieldType::NodeList => Some(SerialFieldState::Text(
            bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        )),
        FieldType::Int => {
            let value = field.endian.read_signed(bytes);
            Some(SerialFieldState::Text(value.to_string()))
        }
        FieldType::Uint8 | FieldType::Duration | FieldType::CallbackId => {
            if field.format == FieldFormat::Hex {
                Some(hex_state_from_bytes(field, bytes))
            } else {
                Some(SerialFieldState::Text(
                    field.endian.read_unsigned(bytes).to_string(),
                ))
            }
        }
        FieldType::Uint16 | FieldType::Uint32 | FieldType::Uint => {
            let value = field.endian.read_unsigned(bytes);
            if field.format == FieldFormat::Hex {
                Some(hex_state_from_bytes(field, bytes))
            } else {
                Some(SerialFieldState::Text(value.to_string()))
            }
        }
        FieldType::Hex | FieldType::CommandClassData => {
            Some(SerialFieldState::Text(spaced_hex(bytes)))
        }
        FieldType::Text => Some(SerialFieldState::Text(
            String::from_utf8_lossy(bytes)
                .trim_end_matches('\0')
                .to_string(),
        )),
    }
}

fn hex_state_from_bytes(field: &FieldDef, bytes: &[u8]) -> SerialFieldState {
    if bytes.is_empty() {
        return SerialFieldState::Text(String::new());
    }
    let value = field.endian.read_unsigned(bytes);
    let full = value.to_be_bytes();
    let width = bytes.len().min(full.len());
    SerialFieldState::Text(spaced_hex(&full[full.len() - width..]))
}

fn restore_zwave_builder(dialog: &mut BuilderDialog, payload: &[u8]) -> bool {
    let original = dialog.clone();
    if !restore_zwave_frame(dialog, payload) {
        *dialog = original;
        return false;
    }
    if dialog.build().ok().as_deref() != Some(payload) {
        *dialog = original;
        return false;
    }
    // Prefer structured command fields when they can reproduce the exact frame.
    let raw = dialog.clone();
    if restore_zwave_command(dialog) && dialog.build().ok().as_deref() == Some(payload) {
        return true;
    }
    *dialog = raw;
    true
}

fn restore_zwave_frame(dialog: &mut BuilderDialog, payload: &[u8]) -> bool {
    if payload.len() < 4
        || payload[0] != hodeco_codec::zwave::FRAME_SOF
        || payload[2] != hodeco_codec::zwave::FRAME_REQUEST
        || usize::from(payload[1]) + 2 != payload.len()
    {
        return false;
    }
    let decoded = hodeco_codec::zwave::decode(payload);
    if decoded.frames.len() != 1
        || decoded.frames[0].checksum_ok != Some(true)
        || decoded.frames[0].range.start != 0
        || decoded.frames[0].range.end != payload.len()
    {
        return false;
    }

    let command = payload[3];
    let params = &payload[4..payload.len() - 1];
    dialog.raw_payload.clear();
    dialog.raw_serial.clear();
    dialog.command_params.clear();
    dialog.serial_params.clear();
    dialog.serial_fields.clear();
    dialog.raw_command.clear();
    dialog.command_class = None;
    dialog.command_id = None;
    match command {
        hodeco_codec::zwave::SERIAL_SEND_DATA => restore_zwave_send_data(dialog, params),
        hodeco_codec::zwave::SERIAL_SEND_DATA_MULTI => {
            restore_zwave_send_data_multi(dialog, params)
        }
        _ => restore_zwave_serial_command(dialog, command, params),
    }
}

fn restore_zwave_send_data(dialog: &mut BuilderDialog, params: &[u8]) -> bool {
    if params.len() < 4 {
        return false;
    }
    let node = params[0];
    let data_len = usize::from(params[1]);
    let data_end = 2 + data_len;
    if data_len == 0 || params.len() != data_end + 2 {
        return false;
    }
    let data = &params[2..data_end];
    let tx_options = params[data_end];
    let callback_id = params[data_end + 1];
    let broadcast = node == codec::BROADCAST_NODE_ID && tx_options == codec::BROADCAST_TX_OPTIONS;
    dialog.operation = if broadcast {
        BuilderOperation::Special(SpecialOperation::Broadcast)
    } else {
        BuilderOperation::SerialCommand(hodeco_codec::zwave::SERIAL_SEND_DATA)
    };
    dialog.node = if broadcast {
        String::new()
    } else {
        node.to_string()
    };
    dialog.nodes.clear();
    dialog.callback_id = callback_id.to_string();
    if !broadcast {
        dialog
            .serial_fields
            .insert("txOptions".to_string(), SerialFieldState::Flags(tx_options));
    }
    dialog.raw_cc = spaced_hex(data);
    true
}

fn restore_zwave_send_data_multi(dialog: &mut BuilderDialog, params: &[u8]) -> bool {
    if params.is_empty() {
        return false;
    }
    let node_count = usize::from(params[0]);
    let nodes_end = 1 + node_count;
    if node_count == 0 || params.len() < nodes_end + 3 {
        return false;
    }
    let data_len = usize::from(params[nodes_end]);
    let data_start = nodes_end + 1;
    let data_end = data_start + data_len;
    if data_len == 0 || params.len() != data_end + 2 {
        return false;
    }
    let nodes = &params[1..nodes_end];
    let data = &params[data_start..data_end];
    let tx_options = params[data_end];
    let callback_id = params[data_end + 1];
    dialog.operation = BuilderOperation::SerialCommand(hodeco_codec::zwave::SERIAL_SEND_DATA_MULTI);
    dialog.node.clear();
    dialog.nodes = nodes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    dialog.callback_id = callback_id.to_string();
    dialog
        .serial_fields
        .insert("txOptions".to_string(), SerialFieldState::Flags(tx_options));
    dialog.raw_cc = spaced_hex(data);
    true
}

fn restore_zwave_serial_command(dialog: &mut BuilderDialog, command: u8, params: &[u8]) -> bool {
    let definitions = definitions::current();
    let Some(definition) = definitions.serial_command(command) else {
        return false;
    };
    if !definition.no_request_parameters && !definition.fields.is_empty() {
        return false;
    }
    dialog.operation = BuilderOperation::SerialCommand(command);
    dialog.serial_params = spaced_hex(params);
    true
}

fn restore_zwave_command(dialog: &mut BuilderDialog) -> bool {
    let Ok(data) = hodeco_codec::parse_hex(&dialog.raw_cc) else {
        return false;
    };
    if data.len() < 2 {
        return false;
    }
    let class_id = data[0];
    let command_id = data[1];
    let params = &data[2..];
    let definitions = definitions::current();
    let Some(_) = definitions.command_class(class_id) else {
        return false;
    };
    let tx_options = dialog.serial_fields.get("txOptions").cloned();
    dialog.command_class = Some(class_id);
    dialog.command_id = Some(command_id);
    dialog.raw_cc.clear();
    dialog.command_params.clear();
    dialog.serial_params.clear();
    dialog.serial_fields.clear();
    dialog.raw_command.clear();
    if let Some(tx_options) = tx_options {
        dialog
            .serial_fields
            .insert("txOptions".to_string(), tx_options);
    }
    let Some(command) = definitions.command(class_id, command_id) else {
        dialog.command_id = None;
        dialog.raw_command = spaced_hex(&data[1..]);
        return true;
    };

    match CommandPreset::from_ids(class_id, command_id) {
        CommandPreset::BasicSet | CommandPreset::BasicReport => {
            if params.len() != 1 {
                return false;
            }
            dialog.value = params[0].to_string();
        }
        CommandPreset::BasicGet => {
            if !params.is_empty() {
                return false;
            }
        }
        CommandPreset::SwitchBinarySet | CommandPreset::SwitchBinaryReport => {
            if params.len() != 1 {
                return false;
            }
            dialog.value = if params[0] == 0 { "0" } else { "255" }.to_string();
        }
        CommandPreset::SwitchMultilevelSet | CommandPreset::SwitchMultilevelReport => {
            if params.len() != 1 {
                return false;
            }
            dialog.value = params[0].to_string();
        }
        CommandPreset::SwitchMultilevelGet | CommandPreset::MeterGet => {
            if !params.is_empty() {
                return false;
            }
        }
        CommandPreset::SwitchBinaryGet => {
            if !params.is_empty() {
                return false;
            }
        }
        CommandPreset::MeterReport => {
            if params.len() < 2 {
                return false;
            }
            dialog.meter_type = format!("0x{:02x}", params[0]);
            dialog.meter_value = spaced_hex(&params[1..]);
        }
        CommandPreset::SceneActivationSet => {
            if params.len() != 2 {
                return false;
            }
            dialog.scene_id = params[0].to_string();
            dialog.dimming = params[1].to_string();
        }
        CommandPreset::Generic => {
            if !command.fields.is_empty() || !params.is_empty() {
                return false;
            }
        }
    }
    true
}

fn command_has_zcl_payload(command_id: u16) -> bool {
    zigbee_codec::current().request_has_role(command_id, FieldRole::Payload)
}

fn zigbee_request_field_name(command_id: u16, role: FieldRole) -> Option<String> {
    zigbee_codec::current()
        .request_field_name_by_role(command_id, role)
        .map(str::to_string)
}

fn zcl_layout(command: &zigbee_codec::ZclCommandDef, to_client: bool) -> &[LayoutField] {
    if to_client {
        &command.response_fields
    } else {
        &command.fields
    }
}

impl BuilderDialog {
    fn reset_serial_fields(&mut self, command_id: u8) {
        self.serial_fields.clear();
        self.serial_params.clear();
        if let Some(command) = definitions::current().serial_command(command_id) {
            for field in command.fields.iter().flat_map(LayoutField::fields) {
                self.serial_fields
                    .insert(field.name.clone(), default_serial_state(field));
            }
        }
    }

    fn reset_zigbee_fields(&mut self, command_id: u16) {
        self.serial_fields.clear();
        self.serial_params.clear();
        if let Some(command) = zigbee_codec::current().command(command_id) {
            for field in command.fields.iter().flat_map(LayoutField::fields) {
                self.serial_fields
                    .insert(field.name.clone(), default_serial_state(field));
            }
        }
        if !command_has_zcl_payload(command_id) {
            self.reset_zcl();
        } else {
            self.sync_zcl_ezsp_fields();
        }
    }

    fn sync_zcl_ezsp_fields(&mut self) {
        let Some(cluster_id) = self.zcl_cluster else {
            return;
        };
        let BuilderOperation::EzspCommand(command_id) = self.operation else {
            return;
        };
        if let Some(field) = zigbee_request_field_name(command_id, FieldRole::Cluster) {
            self.serial_fields.insert(
                field,
                SerialFieldState::Text(spaced_hex(&cluster_id.to_be_bytes())),
            );
        }
    }

    fn reset_zcl(&mut self) {
        self.zcl_cluster = None;
        self.zcl_general = false;
        self.zcl_command = None;
        self.zcl_sequence = "0".to_string();
        self.zcl_direction_to_client = false;
        self.zcl_disable_default_response = false;
        self.zcl_manufacturer.clear();
        self.zcl_fields.clear();
        self.zcl_raw.clear();
    }

    fn select_zcl_cluster(&mut self, cluster_id: Option<u16>) {
        if self.zcl_cluster == cluster_id {
            return;
        }
        let general_command = self.zcl_general.then_some(self.zcl_command).flatten();
        self.zcl_cluster = cluster_id;
        self.zcl_command = general_command;
        self.zcl_fields.clear();
        self.zcl_raw.clear();
        if let Some(cluster_id) = cluster_id {
            self.sync_zcl_ezsp_fields();
            if !self.zcl_general
                && let Some(command_id) = zigbee_codec::current()
                    .cluster(cluster_id)
                    .and_then(|cluster| cluster.commands.first().map(|command| command.id))
            {
                self.select_zcl_command(Some(command_id));
            }
        }
    }

    fn select_zcl_command(&mut self, command_id: Option<u8>) {
        if self.zcl_command == command_id {
            return;
        }
        self.zcl_command = command_id;
        self.zcl_raw.clear();
        self.reload_zcl_fields();
    }

    fn reload_zcl_fields(&mut self) {
        self.zcl_fields.clear();
        if self.zcl_general {
            return;
        }
        if let Some(command) = self.zcl_command_def() {
            for field in zcl_layout(&command, self.zcl_direction_to_client)
                .iter()
                .flat_map(LayoutField::fields)
            {
                self.zcl_fields
                    .insert(field.name.clone(), default_serial_state(field));
            }
        }
    }

    fn normalize_zcl_command(&mut self) {
        if !self.zcl_general {
            return;
        }
        let definitions = zigbee_codec::current();
        let valid = self.zcl_command.is_some_and(|id| {
            definitions
                .general_command(id)
                .is_some_and(|command| command.direction.matches(self.zcl_direction_to_client))
        });
        if valid {
            return;
        }
        let Some(command_id) = definitions
            .general_commands
            .iter()
            .find(|command| command.direction.matches(self.zcl_direction_to_client))
            .map(|command| command.id)
        else {
            self.zcl_command = None;
            return;
        };
        self.zcl_command = Some(command_id);
        self.zcl_fields.clear();
        self.zcl_raw.clear();
    }

    fn zcl_manufacturer_value(&self) -> Result<Option<u16>, String> {
        let text = self.zcl_manufacturer.trim();
        if text.is_empty() {
            return Ok(None);
        }
        parse_u16(text, "ZCL manufacturer code").map(Some)
    }

    fn zcl_command_def(&self) -> Option<zigbee_codec::ZclCommandDef> {
        let cluster_id = self.zcl_cluster?;
        let command_id = self.zcl_command?;
        zigbee_codec::current()
            .cluster(cluster_id)
            .and_then(|cluster| {
                cluster
                    .commands
                    .iter()
                    .find(|command| command.id == command_id)
                    .cloned()
            })
    }

    fn generated_zcl_payload(&self) -> Option<Vec<u8>> {
        if !self.zcl_general {
            self.zcl_cluster?;
        }
        self.build_zcl_payload().ok()
    }

    fn build_zcl_payload(&self) -> Result<Vec<u8>, String> {
        let definitions = zigbee_codec::current();
        let (command_id, payload) = if self.zcl_general {
            let command_id = self
                .zcl_command
                .ok_or_else(|| "select a general ZCL command".to_string())?;
            let command = definitions
                .general_command(command_id)
                .ok_or_else(|| format!("general ZCL command 0x{command_id:02x} is not defined"))?;
            if !command.direction.matches(self.zcl_direction_to_client) {
                return Err(
                    "the selected general ZCL command does not support this direction".to_string(),
                );
            }
            let payload = hodeco_codec::parse_hex(&self.zcl_raw)
                .map_err(|error| format!("invalid ZCL payload: {error}"))?;
            (command_id, payload)
        } else {
            let command = self
                .zcl_command_def()
                .ok_or_else(|| "select a ZCL cluster and command".to_string())?;
            let layout = zcl_layout(&command, self.zcl_direction_to_client);
            let payload = if layout.is_empty() {
                hodeco_codec::parse_hex(&self.zcl_raw)
                    .map_err(|error| format!("invalid ZCL payload: {error}"))?
            } else {
                encode_fields(layout, &self.zcl_fields)?
            };
            (command.id, payload)
        };
        let zcl_frame = &definitions.zcl_frame;
        let sequence = parse_u8(&self.zcl_sequence, "ZCL sequence")?;
        let manufacturer = self.zcl_manufacturer_value()?;
        let mut frame_control = if self.zcl_general {
            zcl_frame.general_frame_type
        } else {
            zcl_frame.cluster_frame_type
        };
        if self.zcl_direction_to_client {
            frame_control |= zcl_frame.direction_to_client_bit;
        }
        if self.zcl_disable_default_response {
            frame_control |= zcl_frame.disable_default_response_bit;
        }
        if manufacturer.is_some() {
            frame_control |= zcl_frame.manufacturer_specific_bit;
        }
        let mut frame = Vec::with_capacity(3 + payload.len() + zcl_frame.manufacturer_code_length);
        frame.push(frame_control);
        if let Some(manufacturer) = manufacturer {
            let bytes = zcl_frame
                .manufacturer_code_endian
                .encode_unsigned(u64::from(manufacturer), zcl_frame.manufacturer_code_length);
            frame.extend_from_slice(&bytes);
        }
        frame.push(sequence);
        frame.push(command_id);
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    fn reset_command_fields(&mut self) {
        self.serial_fields.clear();
        self.serial_params.clear();
        self.command_params.clear();
        self.raw_command.clear();
        if let (Some(class_id), Some(command_id)) = (self.command_class, self.command_id)
            && let Some(command) = definitions::current().command(class_id, command_id)
        {
            for field in command.fields.iter().flat_map(LayoutField::fields) {
                self.serial_fields
                    .insert(field.name.clone(), default_serial_state(field));
            }
        }
    }

    fn select_command_class(&mut self, class_id: Option<u8>) {
        if self.command_class == class_id {
            return;
        }
        self.command_class = class_id;
        let definitions = definitions::current();
        self.command_id = class_id
            .and_then(|id| definitions.command_class(id))
            .and_then(|class| {
                self.command_id
                    .filter(|command| class.commands.iter().any(|item| item.id == *command))
                    .or_else(|| class.commands.first().map(|command| command.id))
            });
        self.reset_command_fields();
    }

    fn select_command(&mut self, command_id: Option<u8>) {
        if self.command_id == command_id {
            return;
        }
        self.command_id = command_id;
        self.reset_command_fields();
    }

    fn command_preset(&self) -> CommandPreset {
        match (self.command_class, self.command_id) {
            (Some(class_id), Some(command_id)) => CommandPreset::from_ids(class_id, command_id),
            _ => CommandPreset::Generic,
        }
    }

    fn command_class_label(&self) -> String {
        match self.command_class {
            Some(class_id) => definitions::current()
                .command_class(class_id)
                .map(command_class_display)
                .unwrap_or_else(|| format!("0x{class_id:02x}")),
            None => "Raw command class bytes".to_string(),
        }
    }

    fn command_label(&self) -> String {
        match (self.command_class, self.command_id) {
            (Some(class_id), Some(command_id)) => definitions::current()
                .command(class_id, command_id)
                .map(|command| format!("{} (0x{class_id:02x}/0x{command_id:02x})", command.name))
                .unwrap_or_else(|| format!("0x{class_id:02x}/0x{command_id:02x}")),
            _ => "Select a command".to_string(),
        }
    }

    fn callback(&self) -> Result<u8, String> {
        parse_u8(&self.callback_id, "callback ID")
    }

    fn tx_options(&self) -> u8 {
        if let Some(value) = self
            .serial_fields
            .get("txOptions")
            .and_then(|state| match state {
                SerialFieldState::Flags(value) => Some(*value),
                _ => None,
            })
        {
            return value;
        }
        let definitions = definitions::current();
        self.operation
            .serial_command_id()
            .and_then(|id| definitions.serial_command(id))
            .and_then(|command| {
                command
                    .fields
                    .iter()
                    .flat_map(LayoutField::fields)
                    .find(|field| field.kind == FieldType::TxOptions)
            })
            .and_then(|field| match default_serial_state(field) {
                SerialFieldState::Flags(value) => Some(value),
                _ => None,
            })
            .unwrap_or(codec::TX_OPTION_ACK)
    }

    fn command_bytes(&self) -> Result<Vec<u8>, String> {
        let value = || parse_u8(&self.value, "value");
        let Some(class_id) = self.command_class else {
            let bytes = hodeco_codec::parse_hex(&self.raw_cc)
                .map_err(|error| format!("invalid command class bytes: {error}"))?;
            if bytes.is_empty() {
                return Err("command class bytes are empty".to_string());
            }
            return Ok(bytes);
        };
        let Some(command_id) = self.command_id else {
            let body = hodeco_codec::parse_hex(&self.raw_command)
                .map_err(|error| format!("invalid command bytes: {error}"))?;
            if body.is_empty() {
                return Err("command bytes are empty".to_string());
            }
            let mut bytes = Vec::with_capacity(1 + body.len());
            bytes.push(class_id);
            bytes.extend_from_slice(&body);
            return Ok(bytes);
        };
        let mut bytes = match CommandPreset::from_ids(class_id, command_id) {
            CommandPreset::BasicSet => codec::basic_set(value()?),
            CommandPreset::BasicGet => codec::basic_get(),
            CommandPreset::BasicReport => codec::basic_report(value()?),
            CommandPreset::SwitchBinarySet => codec::switch_binary_set(parse_bool(&self.value)?),
            CommandPreset::SwitchBinaryGet => codec::switch_binary_get(),
            CommandPreset::SwitchBinaryReport => {
                codec::switch_binary_report(parse_bool(&self.value)?)
            }
            CommandPreset::SwitchMultilevelSet => codec::switch_multilevel_set(value()?),
            CommandPreset::SwitchMultilevelGet => codec::switch_multilevel_get(),
            CommandPreset::SwitchMultilevelReport => codec::switch_multilevel_report(value()?),
            CommandPreset::MeterGet => codec::meter_get(),
            CommandPreset::MeterReport => {
                let meter_type = parse_u8(&self.meter_type, "meter type")?;
                let value = hodeco_codec::parse_hex(&self.meter_value)
                    .map_err(|error| format!("invalid meter value: {error}"))?;
                if value.is_empty() {
                    return Err("meter value is empty".to_string());
                }
                codec::meter_report(meter_type, &value)
            }
            CommandPreset::SceneActivationSet => {
                let scene = parse_u8(&self.scene_id, "scene ID")?;
                let dimming = parse_u8(&self.dimming, "dimming duration")?;
                codec::scene_activation_set(scene, dimming)
            }
            CommandPreset::Generic => {
                let definitions = definitions::current();
                let command = definitions.command(class_id, command_id).ok_or_else(|| {
                    format!("command 0x{command_id:02x} is not defined for class 0x{class_id:02x}")
                })?;
                let mut bytes = vec![class_id, command_id];
                bytes.extend(encode_fields(&command.fields, &self.serial_fields)?);
                bytes
            }
        };
        bytes.extend(
            hodeco_codec::parse_hex(&self.command_params)
                .map_err(|error| format!("invalid command parameters: {error}"))?,
        );
        Ok(bytes)
    }

    pub fn build(&self) -> Result<Vec<u8>, String> {
        let callback = if self.operation.uses_callback() {
            self.callback()?
        } else {
            1
        };
        let result = match self.operation {
            BuilderOperation::RawMessage => {
                let bytes = hodeco_codec::parse_hex(&self.raw_payload)
                    .map_err(|error| format!("invalid raw message: {error}"))?;
                if bytes.is_empty() {
                    return Err("raw message is empty".to_string());
                }
                Ok(bytes)
            }
            BuilderOperation::Special(operation) => match operation {
                SpecialOperation::RawFrame => {
                    let bytes = hodeco_codec::parse_hex(&self.raw_serial)
                        .map_err(|error| format!("invalid raw frame: {error}"))?;
                    codec::complete_raw_frame(&bytes)
                }
                SpecialOperation::Broadcast => {
                    let command = self.command_bytes()?;
                    codec::build_broadcast(&command, callback)
                }
            },
            BuilderOperation::EzspCommand(command_id) => {
                let sequence = parse_u8(&self.zigbee_sequence, "EZSP sequence")?;
                let definitions = zigbee_codec::current();
                let command = definitions.command(command_id);
                let params = match command {
                    Some(command) if command.no_request_parameters => Vec::new(),
                    Some(command) if !command.fields.is_empty() => {
                        let mut states = self.serial_fields.clone();
                        if command_has_zcl_payload(command_id)
                            && (self.zcl_cluster.is_some() || self.zcl_general)
                            && let Some(field) =
                                zigbee_request_field_name(command_id, FieldRole::Payload)
                        {
                            let zcl = self.build_zcl_payload()?;
                            states.insert(field, SerialFieldState::Text(spaced_hex(&zcl)));
                        }
                        encode_fields(&command.fields, &states)?
                    }
                    _ => hodeco_codec::parse_hex(&self.serial_params)
                        .map_err(|error| format!("invalid parameters: {error}"))?,
                };
                Ok(zigbee_codec::build_request(sequence, command_id, &params))
            }
            BuilderOperation::SerialCommand(command_id) => match self.operation.kind() {
                OperationKind::SendData => {
                    let node = parse_u8(&self.node, "node ID")?;
                    let command = self.command_bytes()?;
                    codec::build_send_data(node, &command, self.tx_options(), callback)
                }
                OperationKind::SendDataMulti => {
                    let nodes = parse_nodes(&self.nodes)?;
                    let command = self.command_bytes()?;
                    codec::build_send_data_multi(&nodes, &command, self.tx_options(), callback)
                }
                _ => {
                    let definitions = definitions::current();
                    let command = definitions.serial_command(command_id);
                    let params = match command {
                        Some(command) if command.no_request_parameters => Vec::new(),
                        Some(command) if !command.fields.is_empty() => {
                            encode_serial_fields(command, &self.serial_fields)?
                        }
                        _ => hodeco_codec::parse_hex(&self.serial_params)
                            .map_err(|error| format!("invalid parameters: {error}"))?,
                    };
                    codec::build_request(command_id, &params)
                }
            },
        };
        result.map_err(|error| error.to_string())
    }

    pub fn ttl(&self) -> Result<Option<HumanDuration>, String> {
        let text = self.ttl.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<HumanDuration>()
            .map(Some)
            .map_err(|error| format!("invalid TTL: {error}"))
    }

    fn node_label(&self, services: &[ServiceInfo], node: u8) -> String {
        let keys = self
            .service
            .as_ref()
            .map(|reference| model::entity_service_keys(services, reference))
            .unwrap_or_default();
        match entities::current().node_label(&keys, node) {
            Some(device) => format!("{node:>3}  {}", device.display()),
            None => format!("{node:>3}  (unknown)"),
        }
    }

    fn known_zigbee_devices(&self, services: &[ServiceInfo]) -> Vec<(u16, Option<u8>, String)> {
        let keys = self
            .service
            .as_ref()
            .map(|reference| model::entity_service_keys(services, reference))
            .unwrap_or_default();
        let profile = match self.operation {
            BuilderOperation::EzspCommand(command_id) => {
                let field = zigbee_codec::current()
                    .request_field_by_role(command_id, FieldRole::Profile)
                    .cloned();
                field.and_then(|field| {
                    let state = self.serial_fields.get(&field.name)?;
                    let SerialFieldState::Text(text) = state else {
                        return None;
                    };
                    let width = field.kind.fixed_width().unwrap_or(2);
                    parse_serial_uint(text, width, field.format, &field.label)
                        .ok()
                        .and_then(|value| u16::try_from(value).ok())
                })
            }
            _ => None,
        };
        let mut devices: Vec<(u16, Option<u8>, String)> = entities::current()
            .service_devices(&keys)
            .into_iter()
            .filter_map(|(service_device, device)| {
                if let Some(profile) = profile
                    && service_device
                        .profile
                        .is_some_and(|device_profile| device_profile != profile)
                {
                    return None;
                }
                let nwk = service_device.network_address()?;
                Some((
                    nwk,
                    service_device.endpoint,
                    format!("0x{nwk:04x}  {}", device.display()),
                ))
            })
            .collect();
        devices.sort_by_key(|(nwk, endpoint, _)| (*nwk, *endpoint));
        devices.dedup_by_key(|(nwk, endpoint, _)| (*nwk, *endpoint));
        devices
    }

    fn zigbee_direct_destination(&self) -> bool {
        let BuilderOperation::EzspCommand(command_id) = self.operation else {
            return true;
        };
        let definitions = zigbee_codec::current();
        let Some(field) = definitions.request_field_by_role(command_id, FieldRole::DestinationType)
        else {
            return true;
        };
        let Some(direct) = field.values.iter().find(|value| value.direct) else {
            return false;
        };
        self.serial_fields.get(&field.name).is_none_or(
            |state| matches!(state, SerialFieldState::Enum(value) if *value == direct.value),
        )
    }

    fn zigbee_destination_label(&self, services: &[ServiceInfo]) -> Option<String> {
        let field = zigbee_destination_field(self.operation)?;
        let state = self.serial_fields.get(&field)?;
        let SerialFieldState::Text(text) = state else {
            return None;
        };
        let nwk = parse_hex_u16_text(text)?;
        let keys = self
            .service
            .as_ref()
            .map(|reference| model::entity_service_keys(services, reference))
            .unwrap_or_default();
        let catalog = entities::current();
        let device = catalog.network_device(&keys, nwk)?;
        let BuilderOperation::EzspCommand(command_id) = self.operation else {
            return None;
        };
        let endpoint = zigbee_request_field_name(command_id, FieldRole::DestinationEndpoint)
            .and_then(|name| self.serial_fields.get(&name))
            .and_then(|state| match state {
                SerialFieldState::Text(text) => text.trim().parse::<u8>().ok(),
                _ => None,
            });
        Some(match endpoint {
            Some(endpoint) => format!("0x{nwk:04x}  {} · endpoint {endpoint}", device.display()),
            None => format!("0x{nwk:04x}  {}", device.display()),
        })
    }

    fn known_nodes(&self, services: &[ServiceInfo]) -> Vec<(u8, String)> {
        let keys = self
            .service
            .as_ref()
            .map(|reference| model::entity_service_keys(services, reference))
            .unwrap_or_default();
        let mut nodes: Vec<(u8, String)> = entities::current()
            .service_devices(&keys)
            .into_iter()
            .filter_map(|(service_device, device)| {
                service_device
                    .service_id
                    .as_node_id()
                    .map(|node| (node, format!("{node:>3}  {}", device.display())))
            })
            .collect();
        nodes.sort_by_key(|(node, _)| *node);
        nodes.dedup_by_key(|(node, _)| *node);
        nodes
    }
}

fn operation_items(dialog: &BuilderDialog) -> Vec<(BuilderOperation, String)> {
    operation_items_for(&dialog.protocol)
}

fn operation_items_for(protocol: &str) -> Vec<(BuilderOperation, String)> {
    let definitions = definitions::current();
    let mut items: Vec<(i32, BuilderOperation, String)> = vec![(
        BuilderOperation::RawMessage.priority(),
        BuilderOperation::RawMessage,
        BuilderOperation::RawMessage.label(),
    )];

    for operation in SpecialOperation::ZWAVE {
        if operation.applies_to(protocol) {
            items.push((
                operation.priority(),
                BuilderOperation::Special(operation),
                operation.label().to_string(),
            ));
        }
    }

    if protocol == "zwave" {
        for command in &definitions.serial_commands {
            if command.incoming_only {
                continue;
            }
            items.push((
                command.priority,
                BuilderOperation::SerialCommand(command.id),
                format!("{} (0x{:02x})", command.name, command.id),
            ));
        }
    }

    if protocol == "zigbee" {
        for command in &zigbee_codec::current().commands {
            if command.incoming_only {
                continue;
            }
            items.push((
                command.priority,
                BuilderOperation::EzspCommand(command.id),
                format!("{} (0x{:04x})", command.name, command.id),
            ));
        }
    }

    items.sort_by_key(|(priority, _, _)| *priority);
    items
        .into_iter()
        .map(|(_, operation, label)| (operation, label))
        .collect()
}

fn command_class_display(class: &definitions::CommandClassDef) -> String {
    if class.commands.is_empty() {
        format!("{} (0x{:02x}) · raw only", class.name, class.id)
    } else {
        format!("{} (0x{:02x})", class.name, class.id)
    }
}

fn command_class_items() -> Vec<(Option<u8>, String)> {
    let definitions = definitions::current();
    let mut items: Vec<(Option<u8>, String)> = definitions
        .command_classes
        .iter()
        .map(|class| (Some(class.id), command_class_display(class)))
        .collect();
    items.push((None, "Raw command class bytes".to_string()));
    items
}

fn command_items_for(class_id: u8) -> Vec<(u8, String)> {
    definitions::current()
        .command_class(class_id)
        .map(|class| {
            class
                .commands
                .iter()
                .map(|command| {
                    (
                        command.id,
                        format!("{} (0x{class_id:02x}/0x{:02x})", command.name, command.id),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn default_operation_for(protocol: &str) -> BuilderOperation {
    if protocol == "zigbee" {
        let definitions = zigbee_codec::current();
        if let Some(command) = definitions
            .commands
            .iter()
            .find(|command| command.default_operation && !command.incoming_only)
        {
            return BuilderOperation::EzspCommand(command.id);
        }
        if let Some(command) = definitions
            .commands
            .iter()
            .find(|command| !command.incoming_only)
        {
            return BuilderOperation::EzspCommand(command.id);
        }
        return BuilderOperation::RawMessage;
    }

    if protocol != "zwave" {
        return BuilderOperation::RawMessage;
    }

    let definitions = definitions::current();
    if let Some(command) = definitions
        .serial_commands
        .iter()
        .find(|command| !command.incoming_only && command.default_operation)
    {
        return BuilderOperation::SerialCommand(command.id);
    }
    if let Some(command) = definitions
        .serial_commands
        .iter()
        .find(|command| !command.incoming_only && command.builder == SerialBuilderKind::SendData)
    {
        return BuilderOperation::SerialCommand(command.id);
    }
    if let Some(command) = definitions
        .serial_commands
        .iter()
        .find(|command| !command.incoming_only)
    {
        return BuilderOperation::SerialCommand(command.id);
    }

    BuilderOperation::RawMessage
}

fn service_ttl_default(protocols: &[ProtocolInfo], service: &ServiceInfo) -> Option<HumanDuration> {
    service
        .params
        .get(SERVICE_PARAM_OUTGOING_MAX_TTL)
        .and_then(|value| serde_json::from_value::<HumanDuration>(value.clone()).ok())
        .or_else(|| {
            protocols
                .iter()
                .find(|protocol| protocol.id == service.protocol)
                .and_then(|protocol| {
                    protocol
                        .params
                        .iter()
                        .find(|param| param.name == SERVICE_PARAM_OUTGOING_MAX_TTL)
                })
                .and_then(|param| param.default.clone())
                .and_then(|value| serde_json::from_value::<HumanDuration>(value).ok())
        })
}

fn emphasized_operations_for(protocol: &str) -> Vec<BuilderOperation> {
    let mut operations = vec![BuilderOperation::RawMessage];
    for operation in SpecialOperation::ZWAVE {
        if operation.applies_to(protocol) && operation.emphasized() {
            operations.push(BuilderOperation::Special(operation));
        }
    }
    if protocol == "zwave" {
        operations.extend(
            definitions::current()
                .serial_commands
                .iter()
                .filter(|command| !command.incoming_only && command.emphasize)
                .map(|command| BuilderOperation::SerialCommand(command.id)),
        );
    }
    if protocol == "zigbee" {
        operations.extend(
            zigbee_codec::current()
                .commands
                .iter()
                .filter(|command| !command.incoming_only && command.emphasize)
                .map(|command| BuilderOperation::EzspCommand(command.id)),
        );
    }
    operations
}

fn serial_field_suffix(field: &FieldDef) -> &'static str {
    match field.kind {
        FieldType::Hex | FieldType::CommandClassData => " (hex)",
        FieldType::NodeList => " (node IDs)",
        FieldType::TxOptions => " (flags)",
        _ if field.format == FieldFormat::Hex => " (hex)",
        _ => "",
    }
}

fn show_zcl_composer(
    ui: &mut Ui,
    muted: egui::Color32,
    error_color: egui::Color32,
    dialog: &mut BuilderDialog,
) {
    let definitions = zigbee_codec::current();
    let cluster_text = dialog
        .zcl_cluster
        .and_then(|id| definitions.cluster(id))
        .map(|cluster| format!("{} (0x{:04x})", cluster.name, cluster.id))
        .unwrap_or_else(|| "Raw ZCL payload (hex)".to_string());
    ComboBox::from_id_salt("builder-zcl-cluster")
        .selected_text(cluster_text)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(dialog.zcl_cluster.is_none(), "Raw ZCL payload (hex)")
                .clicked()
            {
                dialog.select_zcl_cluster(None);
            }
            for cluster in &definitions.clusters {
                if ui
                    .selectable_label(
                        dialog.zcl_cluster == Some(cluster.id),
                        format!("{} (0x{:04x})", cluster.name, cluster.id),
                    )
                    .clicked()
                {
                    dialog.select_zcl_cluster(Some(cluster.id));
                }
            }
        });
    if ui
        .checkbox(&mut dialog.zcl_general, "General ZCL command")
        .changed()
    {
        dialog.zcl_command = dialog
            .zcl_general
            .then(|| {
                definitions
                    .general_commands
                    .iter()
                    .find(|command| command.direction.matches(dialog.zcl_direction_to_client))
                    .map(|command| command.id)
            })
            .flatten();
        dialog.zcl_fields.clear();
        dialog.zcl_raw.clear();
    }
    dialog.normalize_zcl_command();
    let general = dialog.zcl_general;
    let cluster = dialog
        .zcl_cluster
        .and_then(|id| definitions.cluster(id))
        .cloned();
    if !general && cluster.is_none() {
        ui.label(
            RichText::new(
                "Choose a cluster to compose the ZCL frame from typed fields, \
                 or keep the raw message-contents field above.",
            )
            .size(13.5)
            .color(muted),
        );
        return;
    }
    if general {
        let commands: Vec<&zigbee_codec::ZclGeneralCommandDef> = definitions
            .general_commands
            .iter()
            .filter(|command| command.direction.matches(dialog.zcl_direction_to_client))
            .collect();
        ui.horizontal(|ui| {
            ui.label(RichText::new("ZCL command").strong());
            let selected = dialog
                .zcl_command
                .and_then(|id| definitions.general_command(id))
                .map(|command| format!("{} (0x{:02x})", command.name, command.id))
                .unwrap_or_else(|| "Select general command".to_string());
            let previous = dialog.zcl_command;
            ComboBox::from_id_salt("builder-zcl-general-command")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for command in &commands {
                        ui.selectable_value(
                            &mut dialog.zcl_command,
                            Some(command.id),
                            format!("{} (0x{:02x})", command.name, command.id),
                        );
                    }
                });
            if dialog.zcl_command != previous {
                dialog.zcl_fields.clear();
                dialog.zcl_raw.clear();
            }
        });
    } else if let Some(cluster) = &cluster {
        if cluster.commands.is_empty() {
            ui.label(
                RichText::new(
                    "This cluster has no cluster commands in the catalog. \
                     Use a general ZCL command or the raw message contents field.",
                )
                .size(13.5)
                .color(muted),
            );
        } else {
            let command_text = dialog
                .zcl_command
                .and_then(|id| cluster.commands.iter().find(|command| command.id == id))
                .map(|command| format!("{} (0x{:02x})", command.name, command.id))
                .unwrap_or_else(|| "Select command".to_string());
            ComboBox::from_id_salt("builder-zcl-command")
                .selected_text(command_text)
                .show_ui(ui, |ui| {
                    for command in &cluster.commands {
                        if ui
                            .selectable_label(
                                dialog.zcl_command == Some(command.id),
                                format!("{} (0x{:02x})", command.name, command.id),
                            )
                            .clicked()
                        {
                            dialog.select_zcl_command(Some(command.id));
                        }
                    }
                });
        }
    }
    if general {
        if let Some(command) = dialog
            .zcl_command
            .and_then(|id| definitions.general_command(id))
            && let Some(description) = &command.description
        {
            ui.label(RichText::new(description).size(13.5).color(muted));
        }
    } else if let Some(command) = cluster.as_ref().and_then(|cluster| {
        dialog
            .zcl_command
            .and_then(|id| cluster.commands.iter().find(|command| command.id == id))
    }) && let Some(description) = &command.description
    {
        ui.label(RichText::new(description).size(13.5).color(muted));
    }
    let mut direction_changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("ZCL sequence").strong());
        widgets::numeric_input(ui, &mut dialog.zcl_sequence, "0–255", 90.0, false);
        ui.checkbox(
            &mut dialog.zcl_disable_default_response,
            "Disable default response",
        );
        direction_changed = ui
            .checkbox(&mut dialog.zcl_direction_to_client, "Server → client")
            .changed();
    });
    if direction_changed {
        dialog.normalize_zcl_command();
        dialog.reload_zcl_fields();
    }
    ui.horizontal(|ui| {
        ui.label("Manufacturer code");
        widgets::hex_input(
            ui,
            &mut dialog.zcl_manufacturer,
            "optional, e.g. 1234",
            false,
            2,
            120.0,
        );
    });
    if let Err(error) = dialog.zcl_manufacturer_value() {
        ui.label(RichText::new(error).size(13.0).color(error_color));
    }
    ui.label(
        RichText::new(
            "The frame control byte is generated from these options. \
             Raw ZCL bytes remain available for commands without a field layout.",
        )
        .size(13.5)
        .color(muted),
    );
    if general {
        ui.label(RichText::new("ZCL command payload (hex)").strong());
        widgets::hex_input(
            ui,
            &mut dialog.zcl_raw,
            "command payload bytes",
            true,
            2,
            f32::INFINITY,
        );
    } else if let Some(command) = dialog.zcl_command_def() {
        let cluster_id = dialog.zcl_cluster.unwrap_or_default();
        let layout = zcl_layout(&command, dialog.zcl_direction_to_client);
        if layout.is_empty() {
            ui.label(RichText::new("ZCL command payload (hex)").strong());
            widgets::hex_input(
                ui,
                &mut dialog.zcl_raw,
                "command payload bytes",
                true,
                2,
                f32::INFINITY,
            );
        } else {
            show_fields_grid(
                ui,
                muted,
                ("builder-zcl-fields", cluster_id, command.id),
                layout,
                &mut dialog.zcl_fields,
                &[],
            );
        }
    } else {
        ui.label(
            RichText::new("Select a ZCL command.")
                .size(13.5)
                .color(muted),
        );
    }
    match dialog.build_zcl_payload() {
        Ok(frame) => {
            ui.label(
                RichText::new(format!("ZCL frame: {}", spaced_hex(&frame)))
                    .monospace()
                    .size(13.0)
                    .color(muted),
            );
        }
        Err(error) => {
            ui.label(RichText::new(error).size(13.5).color(error_color));
        }
    }
}

fn show_fields_grid(
    ui: &mut Ui,
    muted: egui::Color32,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    layout: &[LayoutField],
    states: &mut BTreeMap<String, SerialFieldState>,
    hidden: &[&str],
) {
    let computed = computed_values(layout, states);
    egui::Grid::new(id_salt)
        .num_columns(2)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for field in layout
                .iter()
                .flat_map(LayoutField::fields)
                .filter(|field| !hidden.contains(&field.name.as_str()))
            {
                let optional = if field.required { "" } else { " (optional)" };
                let response = ui.label(format!(
                    "{}{}{optional}",
                    field.label,
                    serial_field_suffix(field)
                ));
                if let Some(description) = &field.description {
                    response.on_hover_text(description);
                }
                if let Some(value) = computed.get(&field.name) {
                    ui.horizontal(|ui| {
                        ui.monospace(value.to_string());
                        ui.label(RichText::new("computed").size(12.5).color(muted));
                    });
                } else {
                    let state = states
                        .entry(field.name.clone())
                        .or_insert_with(|| default_serial_state(field));
                    serial_field_widget(ui, field, state);
                }
                ui.end_row();
            }
        });
}

fn default_serial_state(field: &FieldDef) -> SerialFieldState {
    if let Some(default) = &field.default_value {
        return default_state_from_text(field, default);
    }
    match field.kind {
        FieldType::Bool => SerialFieldState::Bool(false),
        FieldType::Enum => SerialFieldState::Enum(
            field
                .values
                .first()
                .map(|value| value.value)
                .unwrap_or_default(),
        ),
        FieldType::Flags => SerialFieldState::Flags(0),
        FieldType::TxOptions => SerialFieldState::Flags(codec::TX_OPTION_ACK),
        FieldType::Node => SerialFieldState::Text(
            field
                .min
                .unwrap_or(1)
                .clamp(0, i64::from(u8::MAX))
                .to_string(),
        ),
        FieldType::NodeList => SerialFieldState::Text("1".to_string()),
        FieldType::Uint8 | FieldType::Uint16 | FieldType::Uint32 => {
            if field.format == FieldFormat::Hex {
                let width = field.kind.fixed_width().unwrap_or(1);
                SerialFieldState::Text(vec!["00"; width].join(" "))
            } else {
                SerialFieldState::Text(
                    field
                        .min
                        .unwrap_or(0)
                        .clamp(0, i64::from(u32::MAX))
                        .to_string(),
                )
            }
        }
        FieldType::CallbackId if field.format == FieldFormat::Hex => {
            SerialFieldState::Text("01".to_string())
        }
        FieldType::CallbackId | FieldType::Duration => SerialFieldState::Text("1".to_string()),
        FieldType::Int | FieldType::Uint => SerialFieldState::Text("0".to_string()),
        FieldType::Hex | FieldType::CommandClassData => SerialFieldState::Text(
            field
                .length
                .map(|length| vec!["00"; length].join(" "))
                .unwrap_or_default(),
        ),
        FieldType::Text => SerialFieldState::Text(String::new()),
    }
}

fn default_state_from_text(field: &FieldDef, text: &str) -> SerialFieldState {
    match field.kind {
        FieldType::Bool => SerialFieldState::Bool(parse_bool(text).unwrap_or(false)),
        FieldType::Enum => SerialFieldState::Enum(
            field
                .values
                .iter()
                .find(|value| value.name.eq_ignore_ascii_case(text))
                .map(|value| value.value)
                .or_else(|| {
                    parse_serial_uint(text, 1, field.format, &field.label)
                        .ok()
                        .and_then(|value| u8::try_from(value).ok())
                })
                .unwrap_or_else(|| {
                    field
                        .values
                        .first()
                        .map(|value| value.value)
                        .unwrap_or_default()
                }),
        ),
        FieldType::Flags | FieldType::TxOptions => SerialFieldState::Flags(
            parse_serial_uint(text, 1, field.format, &field.label)
                .ok()
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or_default(),
        ),
        _ => SerialFieldState::Text(text.to_string()),
    }
}

fn layout_field_map(layout: &[LayoutField]) -> BTreeMap<&str, &FieldDef> {
    layout
        .iter()
        .flat_map(LayoutField::fields)
        .map(|field| (field.name.as_str(), field))
        .collect()
}

fn has_length_mapping(field: &FieldDef) -> bool {
    field.values.iter().any(|value| value.length.is_some())
}

fn numeric_state_value(field: &FieldDef, state: &SerialFieldState) -> Result<u64, String> {
    match state {
        SerialFieldState::Bool(value) => Ok(u64::from(*value)),
        SerialFieldState::Enum(value) | SerialFieldState::Flags(value) => Ok(u64::from(*value)),
        SerialFieldState::Text(text) => {
            let width = field.kind.fixed_width().unwrap_or(1);
            parse_serial_uint(text, width, field.format, &field.label)
        }
    }
}

fn selected_enum_length(
    field: &FieldDef,
    states: &BTreeMap<String, SerialFieldState>,
) -> Result<usize, String> {
    let state = states
        .get(&field.name)
        .cloned()
        .unwrap_or_else(|| default_serial_state(field));
    let raw = numeric_state_value(field, &state)?;
    field
        .values
        .iter()
        .find(|value| u64::from(value.value) == raw)
        .and_then(|value| value.length)
        .ok_or_else(|| format!("{} does not select a byte length", field.label))
}

fn int_width(
    field: &FieldDef,
    field_map: &BTreeMap<&str, &FieldDef>,
    states: &BTreeMap<String, SerialFieldState>,
) -> Result<usize, String> {
    let width = if let Some(width) = field.length {
        width
    } else {
        let source = field
            .length_from
            .as_ref()
            .ok_or_else(|| format!("{} has no length source", field.label))?;
        let source_field = field_map
            .get(source.as_str())
            .ok_or_else(|| format!("{} references unknown length field {source}", field.label))?;
        selected_enum_length(source_field, states)?
    };
    if !matches!(width, 1 | 2 | 4) {
        return Err(format!(
            "{} width must be 1, 2, or 4 bytes, got {width}",
            field.label
        ));
    }
    Ok(width)
}

fn parse_signed(text: &str, field: &str) -> Result<i64, String> {
    text.trim()
        .replace([' ', ','], "")
        .parse::<i64>()
        .map_err(|_| format!("invalid {field} '{}'", text.trim()))
}

fn encode_signed(value: i64, width: usize, label: &str, endian: Endian) -> Result<Vec<u8>, String> {
    match width {
        1 => i8::try_from(value)
            .map(|value| vec![value as u8])
            .map_err(|_| format!("{label} is out of range for 1 byte")),
        2 => i16::try_from(value)
            .map(|value| endian.encode_signed(i64::from(value), 2))
            .map_err(|_| format!("{label} is out of range for 2 bytes")),
        4 => i32::try_from(value)
            .map(|value| endian.encode_signed(i64::from(value), 4))
            .map_err(|_| format!("{label} is out of range for 4 bytes")),
        _ => Err(format!("{label} has an unsupported width")),
    }
}

fn encode_group(
    group: &PackedGroupDef,
    states: &BTreeMap<String, SerialFieldState>,
    computed: &BTreeMap<String, u64>,
) -> Result<Vec<u8>, String> {
    if !(1..=8).contains(&group.length) {
        return Err(format!(
            "group '{}' has unsupported length {}",
            group.group, group.length
        ));
    }
    let group_bits = group.length * 8;
    let mut raw = 0u64;
    let mut used_mask = 0u64;
    for field in &group.fields {
        let mask = field
            .mask
            .ok_or_else(|| format!("packed field '{}' has no mask", field.name))?;
        let shift = field.shift;
        if mask == 0 || shift >= 64 || shift as usize + mask.count_ones() as usize > group_bits {
            return Err(format!(
                "packed field '{}' has an invalid mask/shift",
                field.name
            ));
        }
        let width = mask.count_ones();
        let expected_mask = (u64::MAX >> (64 - width)) << shift;
        if mask != expected_mask {
            return Err(format!(
                "packed field '{}' mask must be contiguous and match shift",
                field.name
            ));
        }
        if used_mask & mask != 0 {
            return Err(format!(
                "packed field '{}' overlaps another field",
                field.name
            ));
        }
        used_mask |= mask;
        let value = if let Some(value) = computed.get(&field.name) {
            *value
        } else {
            let state = states
                .get(&field.name)
                .cloned()
                .unwrap_or_else(|| default_serial_state(field));
            numeric_state_value(field, &state)?
        };
        let max = mask >> shift;
        if value > max {
            return Err(format!("{} is out of range", field.label));
        }
        raw |= (value << shift) & mask;
    }
    Ok(raw.to_be_bytes()[8 - group.length..].to_vec())
}

fn computed_values(
    layout: &[LayoutField],
    states: &BTreeMap<String, SerialFieldState>,
) -> BTreeMap<String, u64> {
    let field_map = layout_field_map(layout);
    let mut computed = BTreeMap::new();
    for field in layout.iter().flat_map(LayoutField::fields) {
        let state = states
            .get(&field.name)
            .cloned()
            .unwrap_or_else(|| default_serial_state(field));
        if let Some(target) = &field.length_from {
            let source_has_mapping = field_map
                .get(target.as_str())
                .is_some_and(|source| has_length_mapping(source));
            if !source_has_mapping && !matches!(field.kind, FieldType::Int | FieldType::Uint) {
                let length = serial_field_payload(field, &state)
                    .map(|bytes| bytes.len())
                    .unwrap_or_default();
                computed.insert(target.clone(), length as u64);
            }
        }
        if let Some(target) = &field.count_from {
            let count = serial_field_count(field, &state).unwrap_or_default();
            computed.insert(target.clone(), count as u64);
        }
    }
    computed
}

fn encode_fields(
    layout: &[LayoutField],
    states: &BTreeMap<String, SerialFieldState>,
) -> Result<Vec<u8>, String> {
    let field_map = layout_field_map(layout);
    let computed = computed_values(layout, states);
    let mut bytes = Vec::new();
    for item in layout {
        if let LayoutField::Group(group) = item {
            bytes.extend(encode_group(group, states, &computed)?);
            continue;
        }
        let LayoutField::Field(field) = item else {
            unreachable!()
        };

        if field.kind == FieldType::Int {
            let state = states
                .get(&field.name)
                .cloned()
                .unwrap_or_else(|| default_serial_state(field));
            let SerialFieldState::Text(text) = state else {
                return Err(format!("{} must be a signed integer", field.label));
            };
            let value = parse_signed(&text, &field.label)?;
            if let Some(min) = field.min
                && value < min
            {
                return Err(format!("{} is below minimum {min}", field.label));
            }
            if let Some(max) = field.max
                && value > max
            {
                return Err(format!("{} is above maximum {max}", field.label));
            }
            let width = int_width(field, &field_map, states)?;
            bytes.extend(encode_signed(value, width, &field.label, field.endian)?);
            continue;
        }

        if field.kind == FieldType::Uint {
            let state = states
                .get(&field.name)
                .cloned()
                .unwrap_or_else(|| default_serial_state(field));
            let SerialFieldState::Text(text) = state else {
                return Err(format!("{} must be an unsigned integer", field.label));
            };
            let width = int_width(field, &field_map, states)?;
            let value = parse_serial_uint(&text, width, field.format, &field.label)?;
            bytes.extend(field.endian.encode_unsigned(value, width));
            continue;
        }

        if let Some(value) = computed.get(&field.name) {
            encode_serial_number(field, *value, &mut bytes)?;
            continue;
        }

        let state = states
            .get(&field.name)
            .cloned()
            .unwrap_or_else(|| default_serial_state(field));
        let mut payload = serial_field_payload(field, &state)?;
        if let Some(source) = &field.length_from
            && let Some(source_field) = field_map.get(source.as_str())
            && has_length_mapping(source_field)
        {
            let expected = selected_enum_length(source_field, states)?;
            if payload.len() != expected {
                return Err(format!(
                    "{} must be {expected} byte(s), got {}",
                    field.label,
                    payload.len()
                ));
            }
        }
        if field.kind == FieldType::NodeList
            && let Some(expected) = field.length
        {
            if payload.len() > expected {
                return Err(format!(
                    "{} accepts at most {expected} node(s), got {}",
                    field.label,
                    payload.len()
                ));
            }
            payload.resize(expected, 0);
        }
        if let Some(expected) = field.length
            && payload.len() != expected
        {
            return Err(format!(
                "{} must be {expected} byte(s), got {}",
                field.label,
                payload.len()
            ));
        }
        if field.required
            && payload.is_empty()
            && !matches!(
                field.kind,
                FieldType::Bool | FieldType::Enum | FieldType::Flags | FieldType::TxOptions
            )
        {
            return Err(format!("{} is required", field.label));
        }
        bytes.extend(payload);
    }
    Ok(bytes)
}

fn encode_serial_fields(
    command: &SerialCommandDef,
    states: &BTreeMap<String, SerialFieldState>,
) -> Result<Vec<u8>, String> {
    encode_fields(&command.fields, states)
}

fn encode_serial_number(field: &FieldDef, value: u64, bytes: &mut Vec<u8>) -> Result<(), String> {
    let width = field
        .kind
        .fixed_width()
        .ok_or_else(|| format!("{} is not a numeric field", field.label))?;
    let max = match width {
        1 => u64::from(u8::MAX),
        2 => u64::from(u16::MAX),
        4 => u64::from(u32::MAX),
        _ => return Err(format!("{} has an unsupported width", field.label)),
    };
    if value > max {
        return Err(format!("{} is out of range", field.label));
    }
    bytes.extend(field.endian.encode_unsigned(value, width));
    Ok(())
}

fn serial_field_payload(field: &FieldDef, state: &SerialFieldState) -> Result<Vec<u8>, String> {
    match (field.kind, state) {
        (FieldType::Bool, SerialFieldState::Bool(value)) => Ok(vec![u8::from(*value)]),
        (FieldType::Enum, SerialFieldState::Enum(value)) => Ok(vec![*value]),
        (FieldType::Flags | FieldType::TxOptions, SerialFieldState::Flags(value)) => {
            Ok(vec![*value])
        }
        (kind, SerialFieldState::Text(text)) => match kind {
            FieldType::Node => Ok(vec![parse_u8(text, &field.label)?]),
            FieldType::NodeList => parse_nodes(text),
            FieldType::Uint8 => Ok(vec![
                parse_serial_uint(text, 1, field.format, &field.label)? as u8,
            ]),
            FieldType::Uint16 => Ok(field
                .endian
                .encode_unsigned(parse_serial_uint(text, 2, field.format, &field.label)?, 2)),
            FieldType::Uint32 => Ok(field
                .endian
                .encode_unsigned(parse_serial_uint(text, 4, field.format, &field.label)?, 4)),
            FieldType::Bool => Ok(vec![u8::from(parse_bool(text)?)]),
            FieldType::Enum
            | FieldType::Flags
            | FieldType::TxOptions
            | FieldType::Duration
            | FieldType::CallbackId => {
                Ok(vec![
                    parse_serial_uint(text, 1, field.format, &field.label)? as u8,
                ])
            }
            FieldType::Hex | FieldType::CommandClassData => hodeco_codec::parse_hex(text)
                .map_err(|error| format!("invalid {}: {error}", field.label)),
            FieldType::Text => Ok(text.as_bytes().to_vec()),
            FieldType::Int | FieldType::Uint => {
                Err(format!("{} needs a width from lengthFrom", field.label))
            }
        },
        (_, SerialFieldState::Bool(value)) => Ok(vec![u8::from(*value)]),
        (_, SerialFieldState::Enum(value)) => Ok(vec![*value]),
        (_, SerialFieldState::Flags(value)) => Ok(vec![*value]),
    }
}

fn serial_field_count(field: &FieldDef, state: &SerialFieldState) -> Result<usize, String> {
    match (field.kind, state) {
        (FieldType::NodeList, SerialFieldState::Text(text)) => {
            parse_nodes(text).map(|nodes| nodes.len())
        }
        _ => serial_field_payload(field, state).map(|bytes| bytes.len()),
    }
}

fn parse_serial_uint(
    text: &str,
    width: usize,
    format: FieldFormat,
    label: &str,
) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{label} is empty"));
    }
    let value = if format == FieldFormat::Hex || text.starts_with("0x") || text.starts_with("0X") {
        let bytes =
            hodeco_codec::parse_hex(text).map_err(|error| format!("invalid {label}: {error}"))?;
        if bytes.len() != width {
            return Err(format!(
                "{label} must be {width} hex byte(s), got {}",
                bytes.len()
            ));
        }
        bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    } else {
        text.replace([' ', ','], "")
            .parse::<u64>()
            .map_err(|_| format!("invalid {label} '{text}'"))?
    };
    let max = match width {
        1 => u64::from(u8::MAX),
        2 => u64::from(u16::MAX),
        4 => u64::from(u32::MAX),
        _ => return Err(format!("{label} has an unsupported width")),
    };
    if value > max {
        return Err(format!("{label} is out of range"));
    }
    Ok(value)
}

fn serial_field_widget(ui: &mut Ui, field: &FieldDef, state: &mut SerialFieldState) {
    match state {
        SerialFieldState::Text(text) => match field.kind {
            FieldType::Node => {
                widgets::numeric_input(ui, text, "1–232, or 255 broadcast", 140.0, false);
            }
            FieldType::NodeList => {
                widgets::numeric_input(ui, text, "e.g. 2, 3, 5", 220.0, true);
            }
            FieldType::Hex | FieldType::CommandClassData => {
                let single_line = field.length.is_some_and(|length| length <= 4);
                widgets::hex_input(ui, text, "hex bytes", !single_line, 2, f32::INFINITY);
            }
            FieldType::Uint8
            | FieldType::Uint16
            | FieldType::Uint32
            | FieldType::Duration
            | FieldType::CallbackId => {
                if field.format == FieldFormat::Hex {
                    let hint = match field.kind {
                        FieldType::Uint16 => "2 hex bytes",
                        FieldType::Uint32 => "4 hex bytes",
                        _ => "1 hex byte",
                    };
                    widgets::hex_input(ui, text, hint, false, 1, 120.0);
                } else {
                    widgets::numeric_input(ui, text, &range_hint(field), 120.0, false);
                }
            }
            FieldType::Int => {
                let hint = if field.min.is_none() && field.max.is_none() {
                    "signed integer".to_string()
                } else {
                    range_hint(field)
                };
                widgets::signed_numeric_input(ui, text, &hint, 140.0);
            }
            FieldType::Uint => {
                widgets::numeric_input(ui, text, &range_hint(field), 140.0, false);
            }
            FieldType::Text => {
                ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY));
            }
            FieldType::Bool => {
                let mut value = parse_bool(text).unwrap_or(false);
                if ui.checkbox(&mut value, "Enabled").changed() {
                    *state = SerialFieldState::Bool(value);
                }
            }
            FieldType::Enum => {
                let mut value = parse_u8(text, &field.label).unwrap_or_default();
                enum_widget(ui, field, &mut value);
                *state = SerialFieldState::Enum(value);
            }
            FieldType::Flags | FieldType::TxOptions => {
                let mut value = parse_u8(text, &field.label).unwrap_or_default();
                flags_widget(ui, field, &mut value);
                *state = SerialFieldState::Flags(value);
            }
        },
        SerialFieldState::Bool(value) => {
            ui.checkbox(value, "Enabled");
        }
        SerialFieldState::Enum(value) => enum_widget(ui, field, value),
        SerialFieldState::Flags(value) => flags_widget(ui, field, value),
    }
}

fn range_hint(field: &FieldDef) -> String {
    match (field.min, field.max) {
        (Some(min), Some(max)) => format!("{min}–{max}"),
        (Some(min), None) => format!("min {min}"),
        (None, Some(max)) => format!("max {max}"),
        (None, None) => "number".to_string(),
    }
}

fn enum_widget(ui: &mut Ui, field: &FieldDef, value: &mut u8) {
    if field.values.is_empty() {
        let mut text = value.to_string();
        if widgets::numeric_input(ui, &mut text, "0–255", 120.0, false)
            && let Ok(parsed) = text.parse::<u8>()
        {
            *value = parsed;
        }
        return;
    }

    let selected = field
        .values
        .iter()
        .find(|item| item.value == *value)
        .map(|item| format!("{} (0x{:02x})", item.name, item.value))
        .unwrap_or_else(|| format!("0x{value:02x}"));
    ComboBox::from_id_salt(("serial-enum", field.name.as_str()))
        .selected_text(selected)
        .width(220.0)
        .show_ui(ui, |ui| {
            for item in &field.values {
                ui.selectable_value(
                    value,
                    item.value,
                    format!("{} (0x{:02x})", item.name, item.value),
                );
            }
        });
}

fn flags_widget(ui: &mut Ui, field: &FieldDef, value: &mut u8) {
    let options: Vec<(u8, &str)> = if field.values.is_empty() && field.kind == FieldType::TxOptions
    {
        vec![
            (codec::TX_OPTION_ACK, "ACK"),
            (codec::TX_OPTION_LOW_POWER, "LOW_POWER"),
            (codec::TX_OPTION_AUTO_ROUTE, "AUTO_ROUTE"),
            (codec::TX_OPTION_NO_ROUTE, "NO_ROUTE"),
            (codec::TX_OPTION_EXPLORE, "EXPLORE"),
        ]
    } else {
        field
            .values
            .iter()
            .map(|item| (item.value, item.name.as_str()))
            .collect()
    };

    if options.is_empty() {
        let mut text = value.to_string();
        if widgets::numeric_input(ui, &mut text, "0–255", 120.0, false)
            && let Ok(parsed) = text.parse::<u8>()
        {
            *value = parsed;
        }
        return;
    }

    ui.horizontal_wrapped(|ui| {
        for (bit, label) in options {
            let mut enabled = *value & bit != 0;
            if ui.checkbox(&mut enabled, label).changed() {
                if enabled {
                    *value |= bit;
                } else {
                    *value &= !bit;
                }
            }
        }
        ui.label(
            RichText::new(format!("0x{value:02x}"))
                .monospace()
                .color(ui.visuals().weak_text_color()),
        );
    });
}

pub(crate) fn show(app: &mut App, ctx: &Context, dialog: &mut BuilderDialog) -> bool {
    let mut keep_open = true;
    let theme = app.theme;
    let task_message = matches!(&dialog.purpose, BuilderPurpose::TaskMessage { .. });
    let title = if task_message {
        "Compose message"
    } else {
        "Send message"
    };
    let description = if task_message {
        "Build the message stored in this task action. Nothing is sent now."
    } else {
        "Build a payload for the selected service and send it."
    };
    let modal_id = Id::unique("hodeco-builder");
    // The dialog only needs to sit above the app. Keeping it in Middle leaves
    // Foreground combo-box popups on top, so their lists stay interactive.
    let area = egui::Modal::default_area(modal_id).order(egui::Order::Middle);
    let response = egui::Modal::new(modal_id)
        .area(area)
        .frame(widgets::modal_frame(theme))
        .show(ctx, |ui| {
            widgets::dialog_width(ui, 660.0);
            widgets::dialog_header(ui, theme, title, Some(description));

            widgets::dialog_body(ui, 620.0, |ui| {
                ui.label(RichText::new("Service").strong());
                let services: Vec<(ServiceRef, String)> = app
                    .services
                    .iter()
                    .map(|service| {
                        let label = match &service.alias {
                            Some(alias) => {
                                format!("{alias}  ({})", model::service_key_label(service))
                            }
                            None => model::service_key_label(service),
                        };
                        (model::service_ref(service), label)
                    })
                    .collect();
                let selected = dialog
                    .service
                    .as_ref()
                    .and_then(|reference| {
                        services
                            .iter()
                            .find(|(candidate, _)| model::refs_match(candidate, reference))
                            .map(|(_, label)| label.clone())
                    })
                    .unwrap_or_else(|| "Select a service".to_string());
                let mut service_choice = None;
                ComboBox::from_id_salt("builder-service")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (reference, label) in &services {
                            let selected = dialog
                                .service
                                .as_ref()
                                .is_some_and(|current| model::refs_match(current, reference));
                            if ui.selectable_label(selected, label).clicked() {
                                service_choice = Some(reference.clone());
                            }
                        }
                    });
                if let Some(reference) = service_choice {
                    let protocol = model::find_service(&app.services, &reference)
                        .map(|service| service.protocol.clone());
                    dialog.service = Some(reference);
                    if let Some(protocol) = protocol
                        && protocol != dialog.protocol
                    {
                        dialog.protocol = protocol.clone();
                        dialog.operation = BuilderOperation::default_for_protocol(&protocol);
                        dialog.serial_fields.clear();
                        dialog.serial_params.clear();
                        dialog.reset_zcl();
                    }
                }
                if services.is_empty() {
                    ui.label(
                        RichText::new("No services are configured.")
                            .size(13.5)
                            .color(app.palette().amber),
                    );
                }

                ui.add_space(6.0);
                let operations = operation_items(dialog);
                if !operations
                    .iter()
                    .any(|(operation, _)| *operation == dialog.operation)
                {
                    dialog.operation = BuilderOperation::default_for_protocol(&dialog.protocol);
                    dialog.serial_fields.clear();
                    dialog.serial_params.clear();
                    dialog.reset_zcl();
                }
                ui.label(RichText::new("Operation").strong());
                let emphasize = emphasized_operations_for(&dialog.protocol);
                let mut operation_choice = Some(dialog.operation);
                widgets::searchable_combo(
                    ui,
                    "builder-operation",
                    dialog.operation.label(),
                    500.0,
                    &mut operation_choice,
                    widgets::ComboOptions {
                        any_label: None,
                        emphasize: &emphasize,
                    },
                    &operations,
                );
                if let Some(operation) = operation_choice
                    && operation != dialog.operation
                {
                    dialog.operation = operation;
                    dialog.error = None;
                    match operation {
                        BuilderOperation::SerialCommand(id) => dialog.reset_serial_fields(id),
                        BuilderOperation::EzspCommand(id) => dialog.reset_zigbee_fields(id),
                        _ => {}
                    }
                }
                let operation_kind = dialog.operation.kind();

                if dialog.protocol == "zwave" {
                    if operation_kind == OperationKind::Generic
                        && let BuilderOperation::SerialCommand(id) = dialog.operation
                    {
                        ui.add_space(4.0);
                        if let Some(command) = definitions::current().serial_command(id) {
                            if let Some(description) = &command.description {
                                ui.label(
                                    RichText::new(description)
                                        .size(13.5)
                                        .color(app.palette().muted),
                                );
                            }
                            if command.no_request_parameters {
                                ui.label(
                                    RichText::new("This command has no request parameters.")
                                        .size(13.5)
                                        .color(app.palette().muted),
                                );
                            } else if command.fields.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "No field layout is defined for this command. \
                                         Enter its documented parameters below.",
                                    )
                                    .size(13.5)
                                    .color(app.palette().muted),
                                );
                                ui.add_space(6.0);
                                ui.label(RichText::new("Parameters (hex)").strong());
                                widgets::hex_input(
                                    ui,
                                    &mut dialog.serial_params,
                                    "wire-order bytes after the command ID",
                                    true,
                                    2,
                                    f32::INFINITY,
                                );
                            } else {
                                ui.label(RichText::new("Request fields").strong());
                                show_fields_grid(
                                    ui,
                                    app.palette().muted,
                                    ("builder-serial-fields", command.id),
                                    &command.fields,
                                    &mut dialog.serial_fields,
                                    &[],
                                );
                            }
                        }
                        ui.label(
                            RichText::new(
                                "The command ID, frame length, and checksum are added automatically.",
                            )
                            .size(13.5)
                            .color(app.palette().muted),
                        );
                    }

                    if operation_kind == OperationKind::Broadcast {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Broadcast: SendData to node 0xFF with ACK off and NO_ROUTE on.",
                            )
                            .size(13.5)
                            .color(app.palette().muted),
                        );
                    }

                    if dialog.operation.needs_nodes() {
                        let multi = operation_kind == OperationKind::SendDataMulti;
                        ui.add_space(6.0);
                        let known = dialog.known_nodes(&app.services);
                        ui.horizontal(|ui| {
                            ui.label(if multi { "Target nodes" } else { "Target node" });
                            widgets::numeric_input(
                                ui,
                                if multi {
                                    &mut dialog.nodes
                                } else {
                                    &mut dialog.node
                                },
                                if multi {
                                    "1–232, comma separated"
                                } else {
                                    "1–232"
                                },
                                if multi { 220.0 } else { 110.0 },
                                multi,
                            );
                            node_picker(
                                ui,
                                &known,
                                if multi {
                                    &mut dialog.nodes
                                } else {
                                    &mut dialog.node
                                },
                                multi,
                            );
                        });
                        if multi {
                            show_node_hints(ui, app, dialog, true);
                        } else if let Ok(node) = dialog.node.trim().parse::<u8>() {
                            ui.label(
                                RichText::new(dialog.node_label(&app.services, node))
                                    .monospace()
                                    .size(13.5)
                                    .color(app.palette().muted),
                            );
                        }
                    }

                    if dialog.operation.needs_command() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Command class").strong());
                        let class_items = command_class_items();
                        let mut class_choice = Some(dialog.command_class);
                        widgets::searchable_combo(
                            ui,
                            "builder-command-class",
                            dialog.command_class_label(),
                            500.0,
                            &mut class_choice,
                            widgets::ComboOptions {
                                any_label: None,
                                emphasize: &[],
                            },
                            &class_items,
                        );
                        if let Some(class) = class_choice
                            && class != dialog.command_class
                        {
                            dialog.select_command_class(class);
                        }

                        if let Some(class_id) = dialog.command_class {
                            ui.add_space(4.0);
                            ui.label(RichText::new("Command").strong());
                            let command_items = command_items_for(class_id);
                            if command_items.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "No named commands are defined for this class. \
                                         Enter the command ID and parameters below.",
                                    )
                                    .size(13.5)
                                    .color(app.palette().muted),
                                );
                            } else {
                                let mut command_choice = dialog.command_id;
                                ComboBox::from_id_salt(("builder-command", class_id))
                                    .selected_text(dialog.command_label())
                                    .show_ui(ui, |ui| {
                                        for (command_id, label) in &command_items {
                                            if ui
                                                .selectable_label(
                                                    dialog.command_id == Some(*command_id),
                                                    label,
                                                )
                                                .clicked()
                                            {
                                                command_choice = Some(*command_id);
                                            }
                                        }
                                    });
                                if command_choice != dialog.command_id {
                                    dialog.select_command(command_choice);
                                }
                            }
                        }

                        if let Some(class_id) = dialog.command_class {
                            if let Some(command_id) = dialog.command_id {
                                let preset = dialog.command_preset();
                                if preset == CommandPreset::Generic {
                                    let definitions = definitions::current();
                                    if let Some(command) =
                                        definitions.command(class_id, command_id)
                                        && !command.fields.is_empty()
                                    {
                                        ui.label(RichText::new("Command parameters").strong());
                                        show_fields_grid(
                                            ui,
                                            app.palette().muted,
                                            ("builder-command-fields", class_id, command_id),
                                            &command.fields,
                                            &mut dialog.serial_fields,
                                            &[],
                                        );
                                    }
                                } else {
                                    egui::Grid::new("builder-command-fields")
                                        .num_columns(2)
                                        .spacing([14.0, 6.0])
                                        .show(ui, |ui| {
                                            if preset.uses_bool_value() {
                                                ui.label("Value");
                                                let mut on =
                                                    parse_bool(&dialog.value).unwrap_or(true);
                                                if ui.checkbox(&mut on, "On").changed() {
                                                    dialog.value =
                                                        if on { "255" } else { "0" }.to_string();
                                                }
                                                ui.end_row();
                                            } else if preset.uses_value() {
                                                ui.label("Value");
                                                widgets::numeric_input(
                                                    ui,
                                                    &mut dialog.value,
                                                    "0–255",
                                                    110.0,
                                                    false,
                                                );
                                                ui.end_row();
                                            }
                                            if preset.uses_scene() {
                                                ui.label("Scene ID");
                                                widgets::numeric_input(
                                                    ui,
                                                    &mut dialog.scene_id,
                                                    "0–255",
                                                    110.0,
                                                    false,
                                                );
                                                ui.end_row();
                                                ui.label("Dimming duration");
                                                widgets::numeric_input(
                                                    ui,
                                                    &mut dialog.dimming,
                                                    "0–255",
                                                    110.0,
                                                    false,
                                                );
                                                ui.end_row();
                                            }
                                            if preset.uses_meter() {
                                                ui.label("Meter type (hex)");
                                                widgets::hex_input(
                                                    ui,
                                                    &mut dialog.meter_type,
                                                    "e.g. 01",
                                                    false,
                                                    1,
                                                    110.0,
                                                );
                                                ui.end_row();
                                                ui.label("Value (hex)");
                                                widgets::hex_input(
                                                    ui,
                                                    &mut dialog.meter_value,
                                                    "e.g. 21 01",
                                                    false,
                                                    1,
                                                    160.0,
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }
                                ui.add_space(4.0);
                                ui.label("Extra parameters (hex)");
                                widgets::hex_input(
                                    ui,
                                    &mut dialog.command_params,
                                    "optional hex bytes",
                                    false,
                                    2,
                                    f32::INFINITY,
                                );
                                ui.label(
                                    RichText::new(
                                        "Optional bytes appended after the generated command payload.",
                                    )
                                    .size(13.5)
                                    .color(app.palette().muted),
                                );
                            } else {
                                ui.label(RichText::new("Command bytes (hex)").strong());
                                widgets::hex_input(
                                    ui,
                                    &mut dialog.raw_command,
                                    "command ID and parameters, e.g. 01 1e",
                                    false,
                                    2,
                                    f32::INFINITY,
                                );
                                ui.label(
                                    RichText::new(
                                        "The command class byte is added automatically.",
                                    )
                                    .size(13.5)
                                    .color(app.palette().muted),
                                );
                            }
                        } else {
                            ui.label(RichText::new("Command bytes (hex)").strong());
                            widgets::hex_input(
                                ui,
                                &mut dialog.raw_cc,
                                "e.g. 20 01 63",
                                true,
                                2,
                                f32::INFINITY,
                            );
                        }
                    }

                    if dialog.operation.uses_tx_options() {
                        ui.add_space(8.0);
                        let definitions = definitions::current();
                        if let Some(field) = dialog
                            .operation
                            .serial_command_id()
                            .and_then(|id| definitions.serial_command(id))
                            .and_then(|command| {
                                command
                                    .fields
                                    .iter()
                                    .flat_map(LayoutField::fields)
                                    .find(|field| field.kind == FieldType::TxOptions)
                            })
                        {
                            ui.label(RichText::new("TX options").strong());
                            let state = dialog
                                .serial_fields
                                .entry(field.name.clone())
                                .or_insert_with(|| default_serial_state(field));
                            serial_field_widget(ui, field, state);
                        }
                    }
                    if dialog.operation.uses_callback() {
                        ui.horizontal(|ui| {
                            ui.label("Callback ID");
                            widgets::numeric_input(
                                ui,
                                &mut dialog.callback_id,
                                "1–255",
                                80.0,
                                false,
                            );
                        });
                    }

                    if operation_kind == OperationKind::RawFrame {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Complete frame (length and checksum are recalculated)")
                                .strong(),
                        );
                        widgets::hex_input(
                            ui,
                            &mut dialog.raw_serial,
                            "e.g. 01 03 00 15",
                            true,
                            3,
                            f32::INFINITY,
                        );
                    }
                }
                if dialog.protocol == "zigbee"
                    && let BuilderOperation::EzspCommand(command_id) = dialog.operation
                {
                    ui.add_space(4.0);
                    if let Some(command) = zigbee_codec::current().command(command_id) {
                        if let Some(description) = &command.description {
                            ui.label(
                                RichText::new(description)
                                    .size(13.5)
                                    .color(app.palette().muted),
                            );
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("EZSP sequence").strong());
                            widgets::numeric_input(
                                ui,
                                &mut dialog.zigbee_sequence,
                                "0–255",
                                90.0,
                                false,
                            );
                            ui.label(
                                RichText::new(
                                    "Correlates this request with the NCP response.",
                                )
                                .size(13.5)
                                .color(app.palette().muted),
                            );
                        });
                        if command.no_request_parameters {
                            ui.label(
                                RichText::new("This command has no request parameters.")
                                    .size(13.5)
                                    .color(app.palette().muted),
                            );
                        } else if command.fields.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No field layout is defined for this command. \
                                     Enter its EZSP parameters below.",
                                )
                                .size(13.5)
                                .color(app.palette().muted),
                            );
                            ui.add_space(6.0);
                            ui.label(RichText::new("Parameters (hex)").strong());
                            widgets::hex_input(
                                ui,
                                &mut dialog.serial_params,
                                "wire-order bytes after the EZSP command ID",
                                true,
                                2,
                                f32::INFINITY,
                            );
                        } else {
                            let zcl_carrier = command_has_zcl_payload(command_id);
                            let payload_field =
                                zigbee_request_field_name(command_id, FieldRole::Payload);
                            let typed_zcl = zcl_carrier && dialog.zcl_cluster.is_some();
                            let mut display_fields = dialog.serial_fields.clone();
                            if typed_zcl
                                && let (Some(payload_field), Some(zcl)) = (
                                    payload_field.as_deref(),
                                    dialog.generated_zcl_payload(),
                                )
                            {
                                display_fields.insert(
                                    payload_field.to_string(),
                                    SerialFieldState::Text(spaced_hex(&zcl)),
                                );
                            }
                            ui.label(RichText::new("Request fields").strong());
                            let hidden: Vec<&str> = if typed_zcl {
                                payload_field.iter().map(String::as_str).collect()
                            } else {
                                Vec::new()
                            };
                            show_fields_grid(
                                ui,
                                app.palette().muted,
                                ("builder-zigbee-fields", command.id),
                                &command.fields,
                                &mut display_fields,
                                &hidden,
                            );
                            for (name, state) in display_fields {
                                if !typed_zcl || Some(name.as_str()) != payload_field.as_deref() {
                                    dialog.serial_fields.insert(name, state);
                                }
                            }
                            if let Some(field) = zigbee_destination_field(dialog.operation) {
                                ui.add_space(6.0);
                                if dialog.zigbee_direct_destination() {
                                    let known = dialog.known_zigbee_devices(&app.services);
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Known device").strong());
                                        if let Some((nwk, endpoint)) =
                                            zigbee_device_picker(ui, &known)
                                        {
                                            dialog.serial_fields.insert(
                                                field.clone(),
                                                SerialFieldState::Text(spaced_hex(
                                                    &nwk.to_be_bytes(),
                                                )),
                                            );
                                            if let Some(endpoint) = endpoint
                                                && let Some(endpoint_field) =
                                                    zigbee_request_field_name(
                                                        command_id,
                                                        FieldRole::DestinationEndpoint,
                                                    )
                                            {
                                                dialog.serial_fields.insert(
                                                    endpoint_field,
                                                    SerialFieldState::Text(endpoint.to_string()),
                                                );
                                            }
                                        }
                                    });
                                    if let Some(label) =
                                        dialog.zigbee_destination_label(&app.services)
                                    {
                                        ui.label(
                                            RichText::new(label)
                                                .monospace()
                                                .size(13.5)
                                                .color(app.palette().muted),
                                        );
                                    }
                                }
                            }
                            if zcl_carrier {
                                ui.add_space(8.0);
                                ui.separator();
                                ui.label(RichText::new("ZCL payload").strong());
                                show_zcl_composer(
                                    ui,
                                    app.palette().muted,
                                    app.palette().red,
                                    dialog,
                                );
                            }
                        }
                    }
                    ui.label(
                        RichText::new(
                            "The gateway adds ASH framing, CRC, escaping, and ACK handling.",
                        )
                        .size(13.5)
                        .color(app.palette().muted),
                    );
                }

                if operation_kind == OperationKind::RawMessage {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Raw message (hex)").strong());
                    widgets::hex_input(
                        ui,
                        &mut dialog.raw_payload,
                        "e.g. 01 03 00 15",
                        true,
                        3,
                        f32::INFINITY,
                    );
                    ui.label(
                        RichText::new("Sent as-is; no protocol framing or checksum is added.")
                            .size(13.5)
                            .color(app.palette().muted),
                    );
                }

                ui.add_space(8.0);
                let ttl_default = dialog
                    .service
                    .as_ref()
                    .and_then(|reference| model::find_service(&app.services, reference))
                    .and_then(|service| service_ttl_default(&app.protocols, service));
                let mut ttl_value = dialog.ttl.trim().parse::<HumanDuration>().ok();
                if params::duration_override_editor(
                    ui,
                    "builder-ttl",
                    "TTL",
                    &mut ttl_value,
                    ttl_default,
                ) {
                    dialog.ttl = ttl_value.map(|ttl| ttl.to_string()).unwrap_or_default();
                }
                ui.label(
                    RichText::new(format!(
                        "The TTL limits how long the outgoing message may wait in the service queue. Default: {}",
                        ttl_default
                            .map(|ttl| ttl.to_string())
                            .unwrap_or_else(|| "not set".to_string())
                    ))
                    .size(13.5)
                    .color(app.palette().muted),
                );

                ui.add_space(10.0);
                ui.separator();
                ui.label(RichText::new("Preview").strong());
                match dialog.build() {
                    Ok(frame) => {
                        let decoded = match dialog.protocol.as_str() {
                            "zwave" => Some(hodeco_codec::zwave::decode(&frame)),
                            "zigbee" => Some(hodeco_codec::zigbee::decode(&frame)),
                            _ => None,
                        };
                        ui.label(
                            RichText::new(spaced_hex(&frame))
                                .monospace()
                                .color(app.palette().text),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("{} bytes", frame.len()))
                                    .size(13.5)
                                    .color(app.palette().muted),
                            );
                            if let Some(decoded) = &decoded
                                && let Some(summary) = decoded.frames.first()
                            {
                                ui.label(
                                    RichText::new(&summary.summary)
                                        .size(13.5)
                                        .color(app.palette().cyan),
                                );
                                if summary.checksum_ok == Some(true) {
                                    ui.label(
                                        RichText::new("checksum OK")
                                            .size(13.5)
                                            .color(app.palette().green),
                                    );
                                }
                            }
                        });
                        if let Some(decoded) = &decoded {
                            for issue in &decoded.issues {
                                ui.label(
                                    RichText::new(issue).size(13.5).color(app.palette().amber),
                                );
                            }
                        }
                    }
                    Err(_) => {
                        ui.label(
                            RichText::new("Complete the required fields to build the frame.")
                                .size(13.5)
                                .color(app.palette().muted),
                        );
                    }
                }
            });

            let build_error = dialog.build().err();
            let ttl_error = dialog.ttl().err();
            let footer_error = build_error
                .as_deref()
                .or(ttl_error.as_deref())
                .or(dialog.error.as_deref());
            let mut action = None;
            widgets::dialog_footer(ui, theme, footer_error, |ui| {
                if ui.button("Cancel").clicked() {
                    keep_open = false;
                }
                let built = dialog.build();
                let ttl = dialog.ttl();
                let ready = app.gateway.phase() == crate::client::Phase::Ready;
                let service_ready = dialog.service.is_some();
                if task_message {
                    if ui
                        .add_enabled(
                            built.is_ok() && ttl.is_ok() && service_ready,
                            egui::Button::new("Use message"),
                        )
                        .clicked()
                        && let Ok(frame) = &built
                    {
                        action = Some(BuilderOutcome::UseMessage(frame.clone()));
                    }
                } else {
                    let enabled = built.is_ok() && ttl.is_ok() && service_ready && ready;
                    if ui
                        .add_enabled(enabled, egui::Button::new("Send message"))
                        .clicked()
                        && let Ok(frame) = &built
                    {
                        action = Some(BuilderOutcome::Send(frame.clone()));
                    }
                    if !ready {
                        ui.label(
                            RichText::new("sending requires a ready connection")
                                .size(13.5)
                                .color(app.palette().muted),
                        );
                    }
                }
                if let Ok(frame) = built {
                    widgets::copy_with_feedback(ui, &spaced_hex(&frame), "Copy bytes");
                }
            });

            if let Some(outcome) = action
                && let Some(service) = dialog.service.clone()
            {
                match dialog.purpose.clone() {
                    BuilderPurpose::Send => {
                        if let BuilderOutcome::Send(frame) = outcome {
                            match dialog.ttl() {
                                Ok(ttl) => {
                                    app.send_service_payload(&service, frame, ttl);
                                    keep_open = false;
                                }
                                Err(error) => dialog.error = Some(error),
                            }
                        }
                    }
                    BuilderPurpose::TaskMessage { action_index } => match dialog.ttl() {
                        Ok(ttl) => {
                            let payload = match outcome {
                                BuilderOutcome::UseMessage(frame) => Some(Base64Bytes::from(frame)),
                                BuilderOutcome::Send(_) => None,
                            };
                            if let Some(payload) = payload {
                                if app.set_task_action_message(
                                    action_index,
                                    service,
                                    payload,
                                    ttl,
                                ) {
                                    keep_open = false;
                                } else {
                                    dialog.error =
                                        Some("The task action no longer exists.".to_string());
                                }
                            }
                        }
                        Err(error) => dialog.error = Some(error),
                    },
                }
            }
        });

    if response.should_close() {
        keep_open = false;
    }
    keep_open
}

fn zigbee_destination_field(operation: BuilderOperation) -> Option<String> {
    let BuilderOperation::EzspCommand(command_id) = operation else {
        return None;
    };
    zigbee_request_field_name(command_id, FieldRole::Target)
}

fn parse_hex_u16_text(text: &str) -> Option<u16> {
    let bytes = hodeco_codec::parse_hex(text).ok()?;
    match bytes.as_slice() {
        [hi, lo] => Some(u16::from_be_bytes([*hi, *lo])),
        [byte] => Some(u16::from(*byte)),
        _ => None,
    }
}

fn zigbee_device_picker(
    ui: &mut Ui,
    known: &[(u16, Option<u8>, String)],
) -> Option<(u16, Option<u8>)> {
    let mut chosen = None;
    ComboBox::from_id_salt("builder-zigbee-device-picker")
        .selected_text("Known device")
        .width(220.0)
        .show_ui(ui, |ui| {
            if known.is_empty() {
                ui.label(
                    RichText::new("No ZigBee devices for this service")
                        .size(13.5)
                        .color(ui.visuals().weak_text_color()),
                );
                return;
            }
            for (nwk, device_endpoint, label) in known {
                if ui
                    .selectable_label(false, RichText::new(label).monospace())
                    .clicked()
                {
                    chosen = Some((*nwk, *device_endpoint));
                }
            }
        });
    chosen
}

fn node_picker(ui: &mut Ui, known: &[(u8, String)], target: &mut String, multi: bool) {
    let label = if multi {
        "Add known node"
    } else {
        "Known node"
    };
    let mut chosen = None;
    ComboBox::from_id_salt(("builder-node-picker", multi))
        .selected_text(label)
        .width(180.0)
        .show_ui(ui, |ui| {
            if known.is_empty() {
                ui.label(
                    RichText::new("No catalog nodes for this service")
                        .size(13.5)
                        .color(ui.visuals().weak_text_color()),
                );
                return;
            }
            for (node, label) in known {
                if ui
                    .selectable_label(false, RichText::new(label).monospace())
                    .clicked()
                {
                    chosen = Some(*node);
                }
            }
        });
    if let Some(node) = chosen {
        if multi {
            let mut nodes = parse_nodes(target).unwrap_or_default();
            if !nodes.contains(&node) {
                nodes.push(node);
            }
            *target = nodes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
        } else {
            *target = node.to_string();
        }
    }
}

fn show_node_hints(ui: &mut Ui, app: &App, dialog: &BuilderDialog, multi: bool) {
    let Ok(nodes) = parse_nodes(&dialog.nodes) else {
        return;
    };
    if multi {
        let text = nodes
            .iter()
            .map(|node| dialog.node_label(&app.services, *node))
            .collect::<Vec<_>>()
            .join(", ");
        ui.label(RichText::new(text).size(13.5).color(app.palette().muted));
    }
}

fn parse_u8(text: &str, field: &str) -> Result<u8, String> {
    let text = text.trim();
    let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16)
    } else {
        text.parse::<u8>()
    };
    parsed.map_err(|_| format!("invalid {field} '{text}' (expected 0–255)"))
}

fn parse_bool(text: &str) -> Result<bool, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" | "255" | "0xff" => Ok(true),
        "0" | "off" | "false" | "no" | "0x00" => Ok(false),
        other => parse_u8(other, "boolean").map(|value| value != 0),
    }
}

fn parse_u16(text: &str, field: &str) -> Result<u16, String> {
    let text = text.trim();
    let value = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16)
    } else {
        text.parse::<u16>()
    };
    value.map_err(|_| format!("invalid {field} '{text}' (expected 0-65535 or hex)"))
}

fn parse_nodes(text: &str) -> Result<Vec<u8>, String> {
    let mut nodes = Vec::new();
    for part in text.split([',', ' ', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        nodes.push(parse_u8(part, "node ID")?);
    }
    if nodes.is_empty() {
        return Err("enter at least one node ID".to_string());
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_switch_binary_values_as_booleans() {
        assert!(parse_bool("255").unwrap());
        assert!(parse_bool("on").unwrap());
        assert!(!parse_bool("0").unwrap());
        assert!(!parse_bool("off").unwrap());
        assert!(parse_bool("63").unwrap());
    }

    #[test]
    fn generic_protocol_uses_raw_message_default() {
        assert_eq!(
            BuilderOperation::default_for_protocol("other").kind(),
            OperationKind::RawMessage
        );
        assert_eq!(
            BuilderOperation::default_for_protocol("zwave").kind(),
            OperationKind::SendData
        );
    }

    #[test]
    fn restores_zwave_send_data_builder_state() {
        let frame = codec::build_send_data(
            5,
            &codec::switch_binary_set(true),
            codec::TX_OPTION_ACK,
            0x21,
        )
        .unwrap();
        let mut dialog = new_builder(None, &[], &[]);
        assert!(restore_protocol_builder(&mut dialog, &frame));
        assert_eq!(
            dialog.operation,
            BuilderOperation::SerialCommand(hodeco_codec::zwave::SERIAL_SEND_DATA)
        );
        assert_eq!(dialog.node, "5");
        assert_eq!(dialog.command_class, Some(0x25));
        assert_eq!(dialog.command_id, Some(0x01));
        assert_eq!(dialog.value, "255");
        assert_eq!(dialog.build().unwrap(), frame);
    }

    #[test]
    fn restores_unknown_zwave_command_as_raw_command_class() {
        let frame =
            codec::build_send_data(7, &[0xF0, 0x01, 0xaa], codec::TX_OPTION_ACK, 3).unwrap();
        let mut dialog = new_builder(None, &[], &[]);
        assert!(restore_protocol_builder(&mut dialog, &frame));
        assert_eq!(
            dialog.operation,
            BuilderOperation::SerialCommand(hodeco_codec::zwave::SERIAL_SEND_DATA)
        );
        assert_eq!(dialog.command_class, None);
        assert_eq!(dialog.raw_cc, "f0 01 aa");
        assert_eq!(dialog.build().unwrap(), frame);
    }

    #[test]
    fn builds_raw_command_bytes_for_classes_without_named_commands() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.node = "2".to_string();
        dialog.command_class = Some(0x2A);
        dialog.command_id = None;
        dialog.raw_command = "01 1e".to_string();
        assert_eq!(dialog.command_bytes().unwrap(), vec![0x2A, 0x01, 0x1e]);
        assert_eq!(
            dialog.build().unwrap(),
            codec::build_send_data(
                2,
                &[0x2A, 0x01, 0x1e],
                codec::TX_OPTION_ACK | codec::TX_OPTION_AUTO_ROUTE,
                1
            )
            .unwrap()
        );
    }

    #[test]
    fn restores_unknown_command_in_a_known_class_as_raw_bytes() {
        let frame = codec::build_send_data(2, &[0x2A, 0x01, 0x1e], 0, 1).unwrap();
        let mut dialog = new_builder(None, &[], &[]);
        assert!(restore_protocol_builder(&mut dialog, &frame));
        assert_eq!(dialog.command_class, Some(0x2A));
        assert_eq!(dialog.command_id, None);
        assert_eq!(dialog.raw_command, "01 1e");
        assert_eq!(dialog.build().unwrap(), frame);
    }

    #[test]
    fn restores_zwave_send_data_multi_builder_state() {
        let frame = codec::build_send_data_multi(&[2, 7], &codec::basic_set(0x63), 0, 4).unwrap();
        let mut dialog = new_builder(None, &[], &[]);
        assert!(restore_protocol_builder(&mut dialog, &frame));
        assert_eq!(
            dialog.operation,
            BuilderOperation::SerialCommand(hodeco_codec::zwave::SERIAL_SEND_DATA_MULTI)
        );
        assert_eq!(dialog.nodes, "2, 7");
        assert_eq!(dialog.command_class, Some(0x20));
        assert_eq!(dialog.command_id, Some(0x01));
        assert_eq!(dialog.value, "99");
        assert_eq!(dialog.build().unwrap(), frame);
    }

    #[test]
    fn task_message_builder_prefills_hex_as_raw_message() {
        let dialog = new_task_message_builder(
            None,
            &[],
            &[],
            2,
            &Base64Bytes::from(vec![0x01, 0x02]),
            None,
        );
        assert_eq!(
            dialog.purpose,
            BuilderPurpose::TaskMessage { action_index: 2 }
        );
        assert_eq!(dialog.operation, BuilderOperation::RawMessage);
        assert_eq!(dialog.raw_payload, "01 02");
    }

    #[test]
    fn operation_items_follow_the_catalog() {
        let dialog = new_builder(None, &[], &[]);
        let items = operation_items(&dialog);
        let definitions = definitions::current();

        assert!(
            items
                .windows(2)
                .all(|pair| pair[0].0.priority() <= pair[1].0.priority())
        );
        assert!(
            items
                .iter()
                .any(|(operation, _)| *operation == BuilderOperation::RawMessage)
        );
        for operation in SpecialOperation::ZWAVE {
            assert!(
                items
                    .iter()
                    .any(|(item, _)| *item == BuilderOperation::Special(operation)),
                "missing {}",
                operation.label()
            );
        }
        for command in &definitions.serial_commands {
            let operation = BuilderOperation::SerialCommand(command.id);
            if command.incoming_only {
                assert!(
                    !items.iter().any(|(item, _)| *item == operation),
                    "incoming command 0x{:02x} must not be offered",
                    command.id
                );
            } else {
                assert!(
                    items.iter().any(|(item, _)| *item == operation),
                    "missing serial command {}",
                    command.name
                );
            }
        }
    }

    #[test]
    fn builds_generic_serial_api_requests() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.operation = BuilderOperation::SerialCommand(0x05);
        let frame = dialog.build().unwrap();
        assert_eq!(hodeco_codec::hex::compact_hex(&frame), "01030005f9");
    }

    fn serial_dialog(command_id: u8) -> BuilderDialog {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.operation = BuilderOperation::SerialCommand(command_id);
        dialog.reset_serial_fields(command_id);
        dialog
    }

    #[test]
    fn builds_typed_serial_api_fields() {
        let mut dialog = serial_dialog(0x06);
        dialog
            .serial_fields
            .insert("rxTimeout".into(), SerialFieldState::Text("1".into()));
        dialog
            .serial_fields
            .insert("txTimeout".into(), SerialFieldState::Text("2".into()));
        assert_eq!(
            hodeco_codec::hex::compact_hex(&dialog.build().unwrap()),
            "010500060102ff"
        );
    }

    #[test]
    fn computes_length_from_data() {
        let mut dialog = serial_dialog(0x24);
        dialog
            .serial_fields
            .insert("offset".into(), SerialFieldState::Text("00 20".into()));
        dialog
            .serial_fields
            .insert("data".into(), SerialFieldState::Text("20 01 63".into()));
        let definitions = definitions::current();
        let command = definitions.serial_command(0x24).unwrap();
        assert_eq!(
            encode_serial_fields(command, &dialog.serial_fields).unwrap(),
            [0x00, 0x20, 0x03, 0x20, 0x01, 0x63]
        );
    }

    #[test]
    fn computes_node_count_and_data_length() {
        let mut dialog = serial_dialog(0x14);
        dialog
            .serial_fields
            .insert("nodeIds".into(), SerialFieldState::Text("2, 3".into()));
        dialog
            .serial_fields
            .insert("data".into(), SerialFieldState::Text("20 01 ff".into()));
        let definitions = definitions::current();
        let command = definitions.serial_command(0x14).unwrap();
        assert_eq!(
            encode_serial_fields(command, &dialog.serial_fields).unwrap(),
            [0x02, 0x02, 0x03, 0x03, 0x20, 0x01, 0xff, 0x05, 0x01]
        );
    }

    #[test]
    fn encodes_application_node_information_flags_and_length() {
        let mut dialog = serial_dialog(0x03);
        dialog
            .serial_fields
            .insert("deviceOptionsMask".into(), SerialFieldState::Flags(0x03));
        dialog
            .serial_fields
            .insert("generic".into(), SerialFieldState::Text("10".into()));
        dialog
            .serial_fields
            .insert("specific".into(), SerialFieldState::Text("01".into()));
        dialog
            .serial_fields
            .insert("nodeParm".into(), SerialFieldState::Text("25 26".into()));
        let definitions = definitions::current();
        let command = definitions.serial_command(0x03).unwrap();
        assert_eq!(
            encode_serial_fields(command, &dialog.serial_fields).unwrap(),
            [0x03, 0x10, 0x01, 0x02, 0x25, 0x26]
        );
    }

    #[test]
    fn pads_fixed_width_node_lists() {
        let mut dialog = serial_dialog(0x5c);
        dialog
            .serial_fields
            .insert("destinations".into(), SerialFieldState::Text("2, 3".into()));
        let definitions = definitions::current();
        let command = definitions.serial_command(0x5C).unwrap();
        assert_eq!(
            encode_serial_fields(command, &dialog.serial_fields).unwrap(),
            [0x02, 0x03, 0x00, 0x00, 0x00, 0x01]
        );
    }

    #[test]
    fn parses_node_lists() {
        assert_eq!(parse_nodes("2, 7;9").unwrap(), vec![2, 7, 9]);
        assert!(parse_nodes("").is_err());
    }

    #[test]
    fn parses_the_optional_send_ttl() {
        let mut dialog = new_builder(None, &[], &[]);
        assert_eq!(dialog.ttl().unwrap(), None);

        dialog.ttl = "1500 ms".to_string();
        assert_eq!(
            dialog.ttl().unwrap().map(HumanDuration::as_std),
            Some(std::time::Duration::from_millis(1_500))
        );

        dialog.ttl = "not a duration".to_string();
        assert!(dialog.ttl().is_err());
    }

    #[test]
    fn builds_generic_command_class_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.select_command_class(Some(0x70));
        assert_eq!(dialog.command_id, Some(4));
        dialog.serial_fields.insert(
            "parameterNumber".to_string(),
            SerialFieldState::Text("1".to_string()),
        );
        dialog
            .serial_fields
            .insert("size".to_string(), SerialFieldState::Enum(2));
        dialog.serial_fields.insert(
            "value".to_string(),
            SerialFieldState::Text("258".to_string()),
        );
        assert_eq!(
            dialog.command_bytes().unwrap(),
            vec![0x70, 0x04, 0x00, 0x01, 0x02, 0x01, 0x02]
        );
    }

    #[test]
    fn send_data_tx_options_come_from_the_serial_state() {
        let mut dialog = serial_dialog(0x13);
        assert_eq!(
            dialog.tx_options(),
            codec::TX_OPTION_ACK | codec::TX_OPTION_AUTO_ROUTE
        );

        dialog.serial_fields.insert(
            "txOptions".to_string(),
            SerialFieldState::Flags(codec::TX_OPTION_AUTO_ROUTE | codec::TX_OPTION_EXPLORE),
        );
        assert_eq!(
            dialog.tx_options(),
            codec::TX_OPTION_AUTO_ROUTE | codec::TX_OPTION_EXPLORE
        );
    }

    #[test]
    fn encodes_packed_add_node_mode() {
        let mut dialog = serial_dialog(0x4A);
        dialog
            .serial_fields
            .insert("mode".to_string(), SerialFieldState::Enum(1));
        dialog
            .serial_fields
            .insert("highPower".to_string(), SerialFieldState::Bool(true));
        dialog
            .serial_fields
            .insert("networkWide".to_string(), SerialFieldState::Bool(true));
        dialog.serial_fields.insert(
            "callbackId".to_string(),
            SerialFieldState::Text("01".to_string()),
        );

        assert_eq!(
            hodeco_codec::hex::compact_hex(&dialog.build().unwrap()),
            "0105004ac10170"
        );
    }

    #[test]
    fn encodes_packed_configuration_properties() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.select_command_class(Some(0x70));
        dialog.select_command(Some(0x0F));
        for (name, state) in [
            ("parameterNumber", SerialFieldState::Text("1".into())),
            ("size", SerialFieldState::Enum(1)),
            ("format", SerialFieldState::Enum(0)),
            ("readOnly", SerialFieldState::Bool(false)),
            ("alteringCapabilities", SerialFieldState::Bool(false)),
            ("minValue", SerialFieldState::Text("0".into())),
            ("maxValue", SerialFieldState::Text("100".into())),
            ("defaultValue", SerialFieldState::Text("0".into())),
            ("nextParameterNumber", SerialFieldState::Text("2".into())),
            ("advanced", SerialFieldState::Bool(false)),
            ("noBulkSupport", SerialFieldState::Bool(false)),
        ] {
            dialog.serial_fields.insert(name.to_string(), state);
        }

        assert_eq!(
            dialog.command_bytes().unwrap(),
            vec![
                0x70, 0x0F, 0x00, 0x01, 0x01, 0x00, 0x64, 0x00, 0x00, 0x02, 0x00
            ]
        );
    }

    #[test]
    fn builds_thermostat_setpoint_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.select_command_class(Some(0x43));
        assert_eq!(dialog.command_id, Some(1));
        dialog
            .serial_fields
            .insert("setpointType".to_string(), SerialFieldState::Enum(1));
        dialog
            .serial_fields
            .insert("size".to_string(), SerialFieldState::Enum(1));
        dialog.serial_fields.insert(
            "value".to_string(),
            SerialFieldState::Text("21".to_string()),
        );
        assert_eq!(
            dialog.command_bytes().unwrap(),
            vec![0x43, 0x01, 0x01, 0x01, 0x15]
        );

        dialog
            .serial_fields
            .insert("size".to_string(), SerialFieldState::Enum(2));
        dialog.serial_fields.insert(
            "value".to_string(),
            SerialFieldState::Text("-5".to_string()),
        );
        assert_eq!(
            dialog.command_bytes().unwrap(),
            vec![0x43, 0x01, 0x01, 0x02, 0xff, 0xfb]
        );
    }

    #[test]
    fn command_class_catalog_includes_legacy_security_panel_classes() {
        let items = command_class_items();
        assert!(items.iter().any(|(id, _)| *id == Some(0x2E)));
        assert!(items.iter().any(|(id, _)| *id == Some(0x2F)));
    }

    #[test]
    fn builds_zigbee_network_state_request() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_NETWORK_STATE);
        dialog.zigbee_sequence = "7".to_string();
        assert_eq!(dialog.build().unwrap(), vec![7, 0, 0x01, 0x18, 0x00]);
    }

    #[test]
    fn builds_zigbee_permit_joining_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_PERMIT_JOINING);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_PERMIT_JOINING);
        dialog.serial_fields.insert(
            "duration".to_string(),
            SerialFieldState::Text("180".to_string()),
        );
        assert_eq!(dialog.build().unwrap(), vec![0, 0, 0x01, 0x22, 0x00, 180],);
    }

    #[test]
    fn builds_zigbee_message_length_from_contents() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.serial_fields.insert(
            "messageContents".to_string(),
            SerialFieldState::Text("18 05 0A 00 00 10 01".to_string()),
        );
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 7);
        assert_eq!(&frame[21..], &[0x18, 0x05, 0x0a, 0x00, 0x00, 0x10, 0x01]);
    }

    #[test]
    fn builds_zigbee_zcl_payload_from_cluster_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.select_zcl_cluster(Some(0x0008));
        dialog.select_zcl_command(Some(0x00));
        dialog.zcl_fields.insert(
            "level".to_string(),
            SerialFieldState::Text("128".to_string()),
        );
        dialog.zcl_fields.insert(
            "transitionTime".to_string(),
            SerialFieldState::Text("2".to_string()),
        );
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 6);
        assert_eq!(&frame[21..], &[0x01, 0x00, 0x00, 0x80, 0x02, 0x00]);
        assert!(
            zigbee_codec::decode(&frame).summary.contains("level 128"),
            "{:?}",
            zigbee_codec::decode(&frame).summary
        );
    }

    #[test]
    fn zcl_composer_renders_selected_command_fields() {
        let ctx = egui::Context::default();
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.select_zcl_cluster(Some(0x0008));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui.set_width(720.0);
                show_zcl_composer(ui, egui::Color32::GRAY, egui::Color32::RED, &mut dialog);
            },
        );
        output.textures_delta.clear();
        let text: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(shape) => Some(shape.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert!(text.iter().any(|line| line.contains("Move to level")));
        assert!(text.iter().any(|line| line.contains("Level")));
    }

    #[test]
    fn resolves_zigbee_devices_from_the_entity_catalog() {
        use hodeco_contract::domain::{ServiceState, ServiceStatus};

        let services = vec![ServiceInfo {
            alias: Some("ZB".to_string()),
            protocol: "zigbee".to_string(),
            transport: "serial".to_string(),
            entry: "COM5".to_string(),
            status: ServiceStatus {
                state: ServiceState::Running,
                error: None,
                detail: None,
            },
            params: Default::default(),
        }];
        let dialog = new_builder(
            Some(ServiceRef::Alias {
                alias: "ZB".to_string(),
            }),
            &services,
            &[],
        );
        let devices = dialog.known_zigbee_devices(&services);
        assert!(
            devices
                .iter()
                .any(|(nwk, endpoint, _)| *nwk == 0x1234 && *endpoint == Some(1))
        );
    }

    #[test]
    fn zigbee_destination_picker_follows_semantic_roles() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);

        assert_eq!(
            zigbee_destination_field(dialog.operation).as_deref(),
            Some("indexOrDestination")
        );
        assert!(dialog.zigbee_direct_destination());

        dialog
            .serial_fields
            .insert("type".to_string(), SerialFieldState::Enum(1));
        assert!(!dialog.zigbee_direct_destination());

        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_BROADCAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_BROADCAST);
        assert_eq!(zigbee_destination_field(dialog.operation), None);
    }

    #[test]
    fn builds_zigbee_server_to_client_cluster_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.select_zcl_cluster(Some(0x0003));
        dialog.select_zcl_command(Some(0x00));
        dialog.zcl_direction_to_client = true;
        dialog.reload_zcl_fields();
        dialog.zcl_fields.insert(
            "identifyTime".to_string(),
            SerialFieldState::Text("5".to_string()),
        );
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 5);
        assert_eq!(&frame[21..], &[0x09, 0x00, 0x00, 0x05, 0x00]);
    }

    #[test]
    fn restores_zigbee_fielded_request() {
        let mut source = new_builder(None, &[], &[]);
        source.protocol = "zigbee".to_string();
        source.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        source.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        source.serial_fields.insert(
            "indexOrDestination".to_string(),
            SerialFieldState::Text("12 34".to_string()),
        );
        source.serial_fields.insert(
            "clusterId".to_string(),
            SerialFieldState::Text("00 06".to_string()),
        );
        source.serial_fields.insert(
            "destinationEndpoint".to_string(),
            SerialFieldState::Text("01".to_string()),
        );
        source.serial_fields.insert(
            "messageContents".to_string(),
            SerialFieldState::Text("18 05 0A 00 00 10 01".to_string()),
        );
        let payload = source.build().unwrap();

        let mut restored = new_builder(None, &[], &[]);
        assert!(restore_zigbee_builder(&mut restored, &payload));
        assert_eq!(restored.build().unwrap(), payload);
    }

    #[test]
    fn builds_zigbee_general_zcl_command() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.zcl_general = true;
        dialog.zcl_command = Some(0x0a);
        dialog.zcl_direction_to_client = true;
        dialog.zcl_raw = "00 01 00".to_string();
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 6);
        assert_eq!(&frame[21..], &[0x08, 0x00, 0x0a, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn builds_zigbee_zcl_manufacturer_and_direction() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.select_zcl_cluster(Some(0x0006));
        dialog.select_zcl_command(Some(0x01));
        dialog.zcl_direction_to_client = true;
        dialog.zcl_disable_default_response = true;
        dialog.zcl_manufacturer = "0x1234".to_string();
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 5);
        assert_eq!(&frame[21..], &[0x1d, 0x34, 0x12, 0x00, 0x01]);
    }

    #[test]
    fn builds_zigbee_zcl_command_without_fields() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        dialog.operation = BuilderOperation::EzspCommand(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.reset_zigbee_fields(zigbee_codec::EZSP_SEND_UNICAST);
        dialog.select_zcl_cluster(Some(0x0006));
        dialog.select_zcl_command(Some(0x01));
        dialog.zcl_disable_default_response = true;
        let frame = dialog.build().unwrap();
        assert_eq!(frame[20], 3);
        assert_eq!(&frame[21..], &[0x11, 0x00, 0x01]);
    }

    #[test]
    fn restores_zigbee_network_state_request() {
        let mut dialog = new_builder(None, &[], &[]);
        dialog.protocol = "zigbee".to_string();
        let payload = [3, 0, 0x01, 0x18, 0x00];
        assert!(restore_zigbee_builder(&mut dialog, &payload));
        assert_eq!(
            dialog.operation,
            BuilderOperation::EzspCommand(zigbee_codec::EZSP_NETWORK_STATE)
        );
        assert_eq!(dialog.zigbee_sequence, "3");
        assert_eq!(dialog.build().unwrap(), payload);
    }

    #[test]
    fn zigbee_operations_follow_the_catalog() {
        let items = operation_items_for("zigbee");
        assert!(items.iter().any(|(operation, _)| *operation
            == BuilderOperation::EzspCommand(zigbee_codec::EZSP_NETWORK_STATE)));
        assert!(items.iter().any(|(operation, _)| *operation
            == BuilderOperation::EzspCommand(zigbee_codec::EZSP_PERMIT_JOINING)));
        assert_eq!(
            default_operation_for("zigbee"),
            BuilderOperation::EzspCommand(zigbee_codec::EZSP_NETWORK_STATE)
        );
        let emphasized = emphasized_operations_for("zigbee");
        assert!(emphasized.contains(&BuilderOperation::EzspCommand(
            zigbee_codec::EZSP_SEND_UNICAST
        )));
        assert!(emphasized.contains(&BuilderOperation::EzspCommand(
            zigbee_codec::EZSP_PERMIT_JOINING
        )));
        assert!(!emphasized.contains(&BuilderOperation::EzspCommand(zigbee_codec::EZSP_GET_EUI64)));
        assert!(!items.iter().any(|(operation, _)| *operation
            == BuilderOperation::EzspCommand(zigbee_codec::EZSP_INCOMING_MESSAGE_HANDLER)));
    }

    #[test]
    fn resolves_the_default_send_ttl() {
        use hodeco_contract::domain::{ParamDef, ParamType, ServiceState, ServiceStatus};

        let protocol = ProtocolInfo {
            id: "zwave".to_string(),
            name: "Z-Wave".to_string(),
            description: None,
            transports: Vec::new(),
            params: vec![ParamDef::with_default(
                SERVICE_PARAM_OUTGOING_MAX_TTL,
                ParamType::Duration,
                serde_json::json!("10 s"),
                "test default",
            )],
        };
        let mut service = ServiceInfo {
            alias: None,
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM4".to_string(),
            status: ServiceStatus {
                state: ServiceState::Running,
                error: None,
                detail: None,
            },
            params: BTreeMap::new(),
        };

        assert_eq!(
            service_ttl_default(std::slice::from_ref(&protocol), &service)
                .map(|ttl| ttl.to_string()),
            Some("10 s".to_string())
        );

        service.params.insert(
            SERVICE_PARAM_OUTGOING_MAX_TTL.to_string(),
            serde_json::json!("2 s"),
        );
        assert_eq!(
            service_ttl_default(&[protocol], &service).map(|ttl| ttl.to_string()),
            Some("2 s".to_string())
        );
    }
}
