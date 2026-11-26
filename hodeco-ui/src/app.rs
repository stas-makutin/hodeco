//! Application shell, shared state, and gateway event reduction.
//!
//! Views live in `app/views` and only read this state or call the small action
//! helpers below. All transport work is owned by [`Gateway`]; the UI never
//! speaks HTTP.

mod views;

use crate::client::endpoint;
use crate::client::{Gateway, GatewayEvent, MessagePageKind, Phase, RequestTag, RunPageKind, Wake};
use crate::model::{
    self, EXPECTED_PROTOCOL_VERSION, MESSAGE_NOTIFICATION_BATCH, MESSAGE_PAGE_LIMIT, MESSAGE_STATES,
};
use crate::startup::Startup;
use crate::theme::{UiTheme, palette};
use crate::ui::widgets::{self, Toast, Tone};
use chrono::{DateTime, NaiveDate, Utc};
use eframe::Frame;
use egui::{Align, Context, Layout, Margin, RichText, Ui};
use hodeco_codec::definitions::{
    Endian, FieldDef, FieldFormat, FieldRole, FieldType, FilterSlot, LayoutField,
};
use hodeco_codec::filter;
use hodeco_codec::zigbee;
use hodeco_contract::api::notifications::Notification;
use hodeco_contract::api::payloads::{
    CommandsAddResult, CommandsListResult, CommandsUpdateResult, ConfigUpdateResult,
    DiscoveryGetResult, DiscoveryStartResult, MessageTimeRange, MessagesGetResult,
    MessagesListResult, ProtocolParamsResult, ProtocolsListResult, SchedulesAddResult,
    SchedulesListResult, SchedulesPreviewResult, SchedulesSetDefaultsResult,
    SchedulesSolarPreviewResult, SchedulesUpdateResult, ServerInfoResult, ServicesAddResult,
    ServicesAliasResult, ServicesListResult, ServicesParamsResult, ServicesSendResult,
    ServicesStatusResult, TasksAddResult, TasksFireResult, TasksGetRunResult, TasksHistoryResult,
    TasksListResult, TasksRunFeedResult, TasksUpdateResult, TransportsListResult,
};
use hodeco_contract::api::{ApiError, Method};
use hodeco_contract::config::Config;
use hodeco_contract::domain::{
    ActionKind, CommandDefinition, DiscoveryInfo, DiscoveryState, Location, Message,
    MessageDirection, MessageOrder, MessageState, ParamDef, ParamValues, PayloadExpr,
    PayloadOffset, PayloadOp, ProtocolInfo, ScheduleDefinition, ServiceInfo, ServiceRef,
    ServiceState, TaskDefinition, TransportInfo,
};
use hodeco_contract::utils::{Base64Bytes, HumanDuration};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use web_time::Instant;

use views::builder::{BuilderDialog, new_builder, new_task_message_builder};

// ---------------------------------------------------------------------------
// Navigation and shared UI state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Services,
    Messages,
    Tasks,
    Configuration,
    Server,
    Console,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::Services,
        Self::Messages,
        Self::Tasks,
        Self::Configuration,
        Self::Server,
        Self::Console,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Messages => "Messages",
            Self::Tasks => "Tasks",
            Self::Configuration => "Configuration",
            Self::Server => "Server",
            Self::Console => "Console",
        }
    }

    pub fn shortcut(self) -> egui::Key {
        match self {
            Self::Services => egui::Key::Num1,
            Self::Messages => egui::Key::Num2,
            Self::Tasks => egui::Key::Num3,
            Self::Configuration => egui::Key::Num4,
            Self::Server => egui::Key::Num5,
            Self::Console => egui::Key::Num6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServicesTab {
    Services,
    Discovery,
}

impl ServicesTab {
    pub const ALL: [Self; 2] = [Self::Services, Self::Discovery];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Discovery => "Discovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TasksTab {
    Tasks,
    Schedules,
    Commands,
    Runs,
}

impl TasksTab {
    pub const ALL: [Self; 4] = [Self::Tasks, Self::Schedules, Self::Commands, Self::Runs];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Tasks => "Tasks",
            Self::Schedules => "Schedules",
            Self::Commands => "Commands",
            Self::Runs => "Runs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageView {
    Summary,
    Hex,
    Parsed,
    Json,
}

impl MessageView {
    pub const ALL: [Self; 4] = [Self::Summary, Self::Hex, Self::Parsed, Self::Json];

    pub fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Hex => "Hex",
            Self::Parsed => "Parsed",
            Self::Json => "JSON",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageFilters {
    pub states: Vec<MessageState>,
    pub directions: Vec<MessageDirection>,
    pub protocols: Vec<String>,
    pub transports: Vec<String>,
    pub services: Vec<ServiceRef>,
    pub payload_conditions: Vec<PayloadConditionDraft>,
}

impl MessageFilters {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.directions.is_empty()
            && self.protocols.is_empty()
            && self.transports.is_empty()
            && self.services.is_empty()
            && self.payload_conditions.is_empty()
    }

    /// Number of selected filter values. Each selected value is also shown as
    /// its own chip; each payload condition is one chip.
    pub fn count(&self) -> usize {
        self.states.len()
            + self.directions.len()
            + self.protocols.len()
            + self.transports.len()
            + self.services.len()
            + self.payload_conditions.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLeafDraft {
    pub anywhere: bool,
    pub offset: String,
    pub bytes: String,
    pub mask: String,
}

impl Default for RawLeafDraft {
    fn default() -> Self {
        Self {
            anywhere: false,
            offset: "0".to_string(),
            bytes: String::new(),
            mask: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawConditionDraft {
    pub leaves: Vec<RawLeafDraft>,
}

impl RawConditionDraft {
    pub fn new() -> Self {
        Self {
            leaves: vec![RawLeafDraft::default()],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZigbeeFieldMatch {
    pub enabled: bool,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZigbeeConditionDraft {
    pub command: Option<u16>,
    pub network: String,
    pub group: String,
    pub cluster: String,
    pub endpoint: String,
    pub params: String,
    pub zcl_general: bool,
    pub zcl_command: Option<u8>,
    pub zcl_direction_to_client: bool,
    pub zcl_disable_default_response: bool,
    pub zcl_manufacturer: String,
    pub zcl_payload: String,
    pub zcl_fields: BTreeMap<String, ZigbeeFieldMatch>,
    pub ezsp_fields: BTreeMap<String, ZigbeeFieldMatch>,
    pub ezsp_response: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ZigbeeFieldOffset {
    offset: usize,
    width: usize,
    endian: Endian,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ZigbeeOffsets {
    network: Option<ZigbeeFieldOffset>,
    group: Option<ZigbeeFieldOffset>,
    cluster: Option<ZigbeeFieldOffset>,
    endpoint: Option<ZigbeeFieldOffset>,
    zcl: Option<usize>,
}

impl ZigbeeConditionDraft {
    fn offsets(&self) -> ZigbeeOffsets {
        let Some(command) = self.command else {
            return ZigbeeOffsets::default();
        };
        let definitions = zigbee::current();
        let slot_field = |slot| {
            definitions
                .field_layout_by_filter_slot(command, slot)
                .and_then(|(offset, field)| {
                    let width = field.length.or_else(|| field.kind.fixed_width())?;
                    Some(ZigbeeFieldOffset {
                        offset,
                        width,
                        endian: field.endian,
                    })
                })
        };
        ZigbeeOffsets {
            network: slot_field(FilterSlot::Network),
            group: slot_field(FilterSlot::Group),
            cluster: slot_field(FilterSlot::Cluster),
            endpoint: slot_field(FilterSlot::Endpoint),
            zcl: definitions
                .field_layout_by_filter_slot(command, FilterSlot::Payload)
                .map(|(offset, _)| offset),
        }
    }

    pub(crate) fn extra_field_candidates(&self) -> Vec<FieldDef> {
        let Some(command_id) = self.command else {
            return Vec::new();
        };
        let definitions = zigbee::current();
        let Some(command) = definitions.command(command_id) else {
            return Vec::new();
        };
        let layout = if command.incoming_only || command.fields.is_empty() || self.ezsp_response {
            &command.response_fields
        } else {
            &command.fields
        };
        let mut fields = Vec::new();
        for field in layout.iter().filter_map(LayoutField::field) {
            if field.derived
                || field.remaining
                || field.length_from.is_some()
                || field.count_from.is_some()
                || field.filter_slot.is_some()
                || field.role == FieldRole::Length
                || definitions.field_offset(command_id, &field.name).is_none()
            {
                continue;
            }
            if fields
                .iter()
                .any(|existing: &FieldDef| existing.name == field.name)
            {
                continue;
            }
            fields.push(field.clone());
        }
        fields
    }

    fn extra_field_def(&self, name: &str) -> Option<FieldDef> {
        self.extra_field_candidates()
            .into_iter()
            .find(|field| field.name == name)
    }

    pub(crate) fn supports_network(&self) -> bool {
        self.offsets().network.is_some()
    }

    pub(crate) fn supports_group(&self) -> bool {
        self.offsets().group.is_some()
    }

    pub(crate) fn supports_cluster(&self) -> bool {
        self.offsets().cluster.is_some()
    }

    pub(crate) fn supports_endpoint(&self) -> bool {
        self.offsets().endpoint.is_some()
    }

    pub(crate) fn supports_zcl(&self) -> bool {
        self.offsets().zcl.is_some()
    }

    pub fn summary(&self) -> String {
        let command = self
            .command
            .and_then(|id| {
                zigbee::current()
                    .command(id)
                    .map(|command| command.name.clone())
            })
            .unwrap_or_else(|| "any command".to_string());
        let mut parts = vec![format!("ZigBee: {command}")];
        if let Some(network) = parse_hex_u16(&self.network) {
            parts.push(format!("node 0x{network:04x}"));
        }
        if let Some(group) = parse_hex_u16(&self.group) {
            parts.push(format!("group 0x{group:04x}"));
        }
        if let Some(cluster) = parse_hex_u16(&self.cluster) {
            parts.push(format!("cluster 0x{cluster:04x}"));
        }
        if let Some(endpoint) = parse_hex_u8(&self.endpoint) {
            parts.push(format!("endpoint {endpoint}"));
        }
        if !self.params.trim().is_empty() {
            parts.push("EZSP parameters".to_string());
        }
        if self.zcl_general {
            let command = self
                .zcl_command
                .and_then(zigbee::zcl_general_command_name)
                .map(|name| format!(" · {name}"))
                .unwrap_or_default();
            parts.push(format!("ZCL general{command}"));
        } else if let Some(command) = self.zcl_command {
            let definitions = zigbee::current();
            let label = parse_hex_u16(&self.cluster)
                .and_then(|id| definitions.cluster(id))
                .and_then(|cluster| {
                    cluster
                        .commands
                        .iter()
                        .find(|item| item.id == command)
                        .map(|item| format!("{} · {}", cluster.name, item.name))
                })
                .unwrap_or_else(|| format!("ZCL command 0x{command:02x}"));
            parts.push(label);
        }
        let enabled_fields = self
            .zcl_fields
            .values()
            .filter(|field| field.enabled)
            .count();
        if enabled_fields > 0 {
            parts.push(format!("{enabled_fields} ZCL field(s)"));
        }
        let enabled_ezsp_fields = self
            .ezsp_fields
            .values()
            .filter(|field| field.enabled)
            .count();
        if enabled_ezsp_fields > 0 {
            parts.push(format!("{enabled_ezsp_fields} EZSP field(s)"));
        }
        if !self.zcl_payload.trim().is_empty() {
            parts.push("ZCL payload".to_string());
        }
        parts.join(" · ")
    }

    pub fn to_expr(&self) -> Result<PayloadExpr, String> {
        let offsets = self.offsets();
        let mut leaves = Vec::new();
        if let Some(command) = self.command {
            leaves.push(PayloadExpr::leaf(
                PayloadOffset::At(3),
                command.to_le_bytes().to_vec(),
            ));
        }
        if !self.network.trim().is_empty() {
            let Some(field) = offsets.network else {
                return Err("network address is not available for this command".to_string());
            };
            let value = parse_hex_value(&self.network, field.width)
                .ok_or_else(|| "network address must be a 16-bit hex value".to_string())?;
            leaves.push(semantic_leaf(field, value, "network address")?);
        }
        if !self.group.trim().is_empty() {
            let Some(field) = offsets.group else {
                return Err("group ID is not available for this command".to_string());
            };
            let value = parse_hex_value(&self.group, field.width)
                .ok_or_else(|| "group ID must be a 16-bit hex value".to_string())?;
            leaves.push(semantic_leaf(field, value, "group ID")?);
        }
        if !self.cluster.trim().is_empty() {
            let Some(field) = offsets.cluster else {
                return Err("cluster ID is not available for this command".to_string());
            };
            let value = parse_hex_value(&self.cluster, field.width)
                .ok_or_else(|| "cluster must be a 16-bit hex value".to_string())?;
            leaves.push(semantic_leaf(field, value, "cluster ID")?);
        }
        if !self.endpoint.trim().is_empty() {
            let Some(field) = offsets.endpoint else {
                return Err("endpoint is not available for this command".to_string());
            };
            let value = parse_hex_value(&self.endpoint, field.width)
                .ok_or_else(|| "endpoint must be an 8-bit value".to_string())?;
            leaves.push(semantic_leaf(field, value, "endpoint")?);
        }
        if !self.params.trim().is_empty() {
            let bytes = hodeco_codec::parse_hex(&self.params)
                .map_err(|error| format!("invalid parameters: {error}"))?;
            if !bytes.is_empty() {
                leaves.push(PayloadExpr::leaf(
                    PayloadOffset::At(zigbee::EZSP_HEADER_LEN as u32),
                    bytes,
                ));
            }
        }
        for (name, field_match) in &self.ezsp_fields {
            if !field_match.enabled {
                continue;
            }
            let Some(command) = self.command else {
                return Err("EZSP field matching requires a command".to_string());
            };
            let field = self
                .extra_field_def(name)
                .ok_or_else(|| format!("unknown EZSP field '{name}'"))?;
            let offset = zigbee::field_offset(command, name)
                .ok_or_else(|| format!("{} has no fixed frame offset", field.label))?;
            let bytes = encode_field_match_value(&field, &field_match.value)?;
            if let Some(width) = field.length.or_else(|| field.kind.fixed_width())
                && bytes.len() != width
            {
                return Err(format!(
                    "{} must be {width} byte(s), got {}",
                    field.label,
                    bytes.len()
                ));
            }
            leaves.push(PayloadExpr::leaf(PayloadOffset::At(offset as u32), bytes));
        }
        leaves.extend(self.zcl_leaves(offsets.zcl)?);
        if leaves.is_empty() {
            return Err("select at least one field to match".to_string());
        }
        Ok(PayloadExpr::and(leaves))
    }

    fn zcl_leaves(&self, zcl_offset: Option<usize>) -> Result<Vec<PayloadExpr>, String> {
        let typed = self.zcl_fields.values().any(|field| field.enabled);
        let raw = self.zcl_payload.trim();
        let has_command = self.zcl_general || self.zcl_command.is_some();
        if !typed && !has_command && raw.is_empty() {
            return Ok(Vec::new());
        }
        let Some(zcl_offset) = zcl_offset else {
            return Err("this EZSP command has no fixed ZCL message-contents field".to_string());
        };
        if !has_command {
            if typed {
                return Err("select a ZCL command before matching typed fields".to_string());
            }
            let bytes = hodeco_codec::parse_hex(raw)
                .map_err(|error| format!("invalid ZCL payload: {error}"))?;
            return Ok(vec![PayloadExpr::leaf(
                PayloadOffset::At(zcl_offset as u32),
                bytes,
            )]);
        }
        if self.zcl_general && self.zcl_command.is_none() {
            return Err("select a general ZCL command".to_string());
        }
        let definitions = zigbee::current();
        let zcl_frame = &definitions.zcl_frame;
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
        let mut leaves = vec![PayloadExpr::masked_leaf(
            PayloadOffset::At(zcl_offset as u32),
            vec![frame_control],
            vec![zcl_frame.frame_control_mask],
        )];
        let mut cursor = zcl_offset + 1;
        if let Some(manufacturer) = manufacturer {
            let bytes = zcl_frame
                .manufacturer_code_endian
                .encode_unsigned(u64::from(manufacturer), zcl_frame.manufacturer_code_length);
            leaves.push(PayloadExpr::leaf(PayloadOffset::At(cursor as u32), bytes));
            cursor += zcl_frame.manufacturer_code_length;
        }
        cursor += 1; // ZCL sequence is intentionally wildcard.
        let command_id = self
            .zcl_command
            .ok_or_else(|| "select a ZCL command".to_string())?;
        if self.zcl_general {
            let command = definitions
                .general_command(command_id)
                .ok_or_else(|| format!("general ZCL command 0x{command_id:02x} is not defined"))?;
            if !command.direction.matches(self.zcl_direction_to_client) {
                return Err(
                    "the selected general ZCL command does not support this direction".to_string(),
                );
            }
        }
        leaves.push(PayloadExpr::leaf(
            PayloadOffset::At(cursor as u32),
            vec![command_id],
        ));
        cursor += 1;
        let mut payload_end = cursor;
        if typed {
            if self.zcl_general {
                return Err(
                    "general ZCL commands use the raw payload field, not typed fields".to_string(),
                );
            }
            let cluster_id = parse_hex_u16(&self.cluster)
                .ok_or_else(|| "cluster must be set for typed ZCL fields".to_string())?;
            let command = definitions
                .cluster(cluster_id)
                .and_then(|cluster| {
                    cluster
                        .commands
                        .iter()
                        .find(|command| command.id == command_id)
                        .cloned()
                })
                .ok_or_else(|| {
                    format!(
                        "ZCL command 0x{command_id:02x} is not defined for cluster 0x{cluster_id:04x}"
                    )
                })?;
            let layout = if self.zcl_direction_to_client {
                &command.response_fields
            } else {
                &command.fields
            };
            let spans = zcl_field_spans(layout).ok_or_else(|| {
                "this ZCL command has variable-length fields; use the raw ZCL payload".to_string()
            })?;
            for (name, field_match) in &self.zcl_fields {
                if !field_match.enabled {
                    continue;
                }
                let (field, offset, width) = spans
                    .iter()
                    .find(|(field, _, _)| field.name == *name)
                    .ok_or_else(|| format!("unknown ZCL field '{name}'"))?;
                let bytes = encode_field_match_value(field, &field_match.value)?;
                if bytes.len() != *width {
                    return Err(format!(
                        "{} must be {} byte(s), got {}",
                        field.label,
                        width,
                        bytes.len()
                    ));
                }
                leaves.push(PayloadExpr::leaf(
                    PayloadOffset::At((cursor + offset) as u32),
                    bytes,
                ));
                payload_end = payload_end.max(cursor + offset + width);
            }
        }
        if !raw.is_empty() {
            let bytes = hodeco_codec::parse_hex(raw)
                .map_err(|error| format!("invalid ZCL payload: {error}"))?;
            leaves.push(PayloadExpr::leaf(
                PayloadOffset::At(payload_end as u32),
                bytes,
            ));
        }
        Ok(leaves)
    }

    fn zcl_manufacturer_value(&self) -> Result<Option<u16>, String> {
        let text = self.zcl_manufacturer.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let value = parse_match_number(text)
            .filter(|value| *value <= u64::from(u16::MAX))
            .ok_or_else(|| "ZCL manufacturer code must be 0-65535 or 0x0000-0xffff".to_string())?;
        Ok(Some(value as u16))
    }

    pub fn leaf_count(&self) -> usize {
        usize::from(self.command.is_some())
            + usize::from(!self.network.trim().is_empty())
            + usize::from(!self.group.trim().is_empty())
            + usize::from(!self.cluster.trim().is_empty())
            + usize::from(!self.endpoint.trim().is_empty())
            + usize::from(!self.params.trim().is_empty())
            + self
                .ezsp_fields
                .values()
                .filter(|field| field.enabled)
                .count()
            + self.zcl_leaf_count()
    }

    fn zcl_leaf_count(&self) -> usize {
        let typed = self
            .zcl_fields
            .values()
            .filter(|field| field.enabled)
            .count();
        let raw = self.zcl_payload.trim();
        let has_command = self.zcl_general || self.zcl_command.is_some();
        if typed == 0 && !has_command && raw.is_empty() {
            return 0;
        }
        if !has_command {
            return usize::from(!raw.is_empty());
        }
        usize::from(true) // frame control
            + usize::from(!self.zcl_manufacturer.trim().is_empty())
            + usize::from(self.zcl_command.is_some())
            + typed
            + usize::from(!raw.is_empty())
    }

    pub fn byte_count(&self) -> usize {
        let offsets = self.offsets();
        let mut bytes = 0;
        if self.command.is_some() {
            bytes += 2;
        }
        if !self.network.trim().is_empty() {
            bytes += offsets.network.map(|field| field.width).unwrap_or(0);
        }
        if !self.group.trim().is_empty() {
            bytes += offsets.group.map(|field| field.width).unwrap_or(0);
        }
        if !self.cluster.trim().is_empty() {
            bytes += offsets.cluster.map(|field| field.width).unwrap_or(0);
        }
        if !self.endpoint.trim().is_empty() {
            bytes += offsets.endpoint.map(|field| field.width).unwrap_or(0);
        }
        bytes += hodeco_codec::parse_hex(&self.params)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        for (name, field_match) in &self.ezsp_fields {
            if field_match.enabled
                && let Some(field) = self.extra_field_def(name)
            {
                bytes += encode_field_match_value(&field, &field_match.value)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0);
            }
        }
        bytes + self.zcl_byte_count()
    }

    fn zcl_byte_count(&self) -> usize {
        let typed = self
            .zcl_fields
            .values()
            .filter(|field| field.enabled)
            .count();
        let raw = self.zcl_payload.trim();
        let has_command = self.zcl_general || self.zcl_command.is_some();
        if typed == 0 && !has_command && raw.is_empty() {
            return 0;
        }
        if !has_command {
            return hodeco_codec::parse_hex(raw)
                .map(|bytes| bytes.len())
                .unwrap_or(0);
        }
        1 + usize::from(!self.zcl_manufacturer.trim().is_empty()) * 2
            + usize::from(self.zcl_command.is_some())
            + typed
            + hodeco_codec::parse_hex(raw)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
    }

    pub fn max_leaf_bytes(&self) -> usize {
        let offsets = self.offsets();
        let mut max = usize::from(self.command.is_some()) * 2;
        max = max.max(
            usize::from(!self.network.trim().is_empty())
                * offsets.network.map(|field| field.width).unwrap_or(0),
        );
        max = max.max(
            usize::from(!self.group.trim().is_empty())
                * offsets.group.map(|field| field.width).unwrap_or(0),
        );
        max = max.max(
            usize::from(!self.cluster.trim().is_empty())
                * offsets.cluster.map(|field| field.width).unwrap_or(0),
        );
        max = max.max(
            usize::from(!self.endpoint.trim().is_empty())
                * offsets.endpoint.map(|field| field.width).unwrap_or(0),
        );
        max = max.max(
            hodeco_codec::parse_hex(&self.params)
                .map(|bytes| bytes.len())
                .unwrap_or(0),
        );
        for (name, field_match) in &self.ezsp_fields {
            if field_match.enabled
                && let Some(field) = self.extra_field_def(name)
            {
                max = max.max(
                    encode_field_match_value(&field, &field_match.value)
                        .map(|bytes| bytes.len())
                        .unwrap_or(0),
                );
            }
        }
        max.max(
            hodeco_codec::parse_hex(self.zcl_payload.trim())
                .map(|bytes| bytes.len())
                .unwrap_or(0),
        )
        .max(2)
    }
}

fn parse_match_number(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(&hex.replace(' ', ""), 16).ok()
    } else {
        text.replace([' ', ','], "").parse::<u64>().ok()
    }
}

fn parse_field_number(field: &FieldDef, text: &str) -> Option<u64> {
    if field.format == FieldFormat::Hex {
        let text = text
            .trim()
            .trim_start_matches("0x")
            .replace([' ', ':', '-', '_'], "");
        if text.is_empty() {
            return None;
        }
        u64::from_str_radix(&text, 16).ok()
    } else {
        parse_match_number(text)
    }
}

pub(crate) fn zcl_field_spans(layout: &[LayoutField]) -> Option<Vec<(FieldDef, usize, usize)>> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for item in layout {
        let LayoutField::Field(field) = item else {
            return None;
        };
        if field.derived {
            continue;
        }
        if field.remaining || field.length_from.is_some() || field.count_from.is_some() {
            break;
        }
        let width = field.length.or_else(|| field.kind.fixed_width())?;
        spans.push((field.as_ref().clone(), cursor, width));
        cursor += width;
    }
    Some(spans)
}

fn encode_field_match_value(field: &FieldDef, text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{} is empty", field.label));
    }
    let named_value = || {
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
        FieldType::Enum | FieldType::Flags | FieldType::Uint8 => {
            let value = named_value()
                .or_else(|| parse_field_number(field, text))
                .filter(|value| *value <= u64::from(u8::MAX))
                .ok_or_else(|| format!("{} is not a valid 8-bit value", field.label))?;
            Ok(vec![value as u8])
        }
        FieldType::Duration | FieldType::TxOptions | FieldType::CallbackId | FieldType::Node => {
            let value = named_value()
                .or_else(|| parse_field_number(field, text))
                .filter(|value| *value <= u64::from(u8::MAX))
                .ok_or_else(|| format!("{} is not a valid 8-bit value", field.label))?;
            Ok(vec![value as u8])
        }
        FieldType::NodeList => Err(format!("{} cannot be matched as one field", field.label)),
        FieldType::Uint16 => {
            let value = named_value()
                .or_else(|| parse_field_number(field, text))
                .filter(|value| *value <= u64::from(u16::MAX))
                .ok_or_else(|| format!("{} is not a valid 16-bit value", field.label))?;
            Ok(field.endian.encode_unsigned(value, 2))
        }
        FieldType::Uint32 => {
            let value = named_value()
                .or_else(|| parse_field_number(field, text))
                .filter(|value| *value <= u64::from(u32::MAX))
                .ok_or_else(|| format!("{} is not a valid 32-bit value", field.label))?;
            Ok(field.endian.encode_unsigned(value, 4))
        }
        FieldType::Int => {
            let value = text
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
            let value = parse_field_number(field, text)
                .ok_or_else(|| format!("{} is not an unsigned integer", field.label))?;
            let width = field
                .length
                .ok_or_else(|| format!("{} has no fixed width", field.label))?;
            Ok(field.endian.encode_unsigned(value, width))
        }
        FieldType::Hex | FieldType::CommandClassData => hodeco_codec::parse_hex(text)
            .map_err(|error| format!("invalid {}: {error}", field.label)),
        FieldType::Text => Ok(text.as_bytes().to_vec()),
    }
}

pub(crate) fn parse_hex_u16(text: &str) -> Option<u16> {
    let text = text
        .trim()
        .trim_start_matches("0x")
        .replace([' ', ':', '-'], "");
    if text.is_empty() {
        return None;
    }
    u16::from_str_radix(&text, 16).ok()
}

fn parse_hex_value(text: &str, width: usize) -> Option<u64> {
    let text = text
        .trim()
        .trim_start_matches("0x")
        .replace([' ', ':', '-'], "");
    if text.is_empty() {
        return None;
    }
    let value = u64::from_str_radix(&text, 16).ok()?;
    if width > 0 && width < 8 && value >= (1u64 << (width * 8)) {
        return None;
    }
    Some(value)
}

fn semantic_leaf(field: ZigbeeFieldOffset, value: u64, label: &str) -> Result<PayloadExpr, String> {
    if field.width == 0 || field.width > 8 {
        return Err(format!("{label} has an unsupported width"));
    }
    let max = if field.width >= 8 {
        u64::MAX
    } else {
        (1u64 << (field.width * 8)) - 1
    };
    if value > max {
        return Err(format!("{label} does not fit in {} byte(s)", field.width));
    }
    Ok(PayloadExpr::leaf(
        PayloadOffset::At(field.offset as u32),
        field.endian.encode_unsigned(value, field.width),
    ))
}

pub(crate) fn parse_hex_u8(text: &str) -> Option<u8> {
    let text = text
        .trim()
        .trim_start_matches("0x")
        .replace([' ', ':', '-'], "");
    if text.is_empty() {
        return None;
    }
    u8::from_str_radix(&text, 16).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadConditionDraft {
    Frame(filter::FilterSpec),
    Zigbee(ZigbeeConditionDraft),
    Raw(RawConditionDraft),
}

impl PayloadConditionDraft {
    pub fn summary(&self) -> String {
        match self {
            Self::Frame(spec) => spec.summary(),
            Self::Zigbee(spec) => spec.summary(),
            Self::Raw(raw) => {
                if raw.leaves.is_empty() {
                    "Raw bytes: no leaves".to_string()
                } else {
                    format!(
                        "Raw bytes: {} match sequence{}",
                        raw.leaves.len(),
                        if raw.leaves.len() == 1 { "" } else { "s" }
                    )
                }
            }
        }
    }

    pub fn to_expr(&self) -> Result<PayloadExpr, String> {
        let expression = match self {
            Self::Frame(spec) => model::to_payload_expr(spec).map_err(|error| error.to_string())?,
            Self::Zigbee(spec) => spec.to_expr()?,
            Self::Raw(raw) => {
                if raw.leaves.is_empty() {
                    return Err("a raw condition needs at least one leaf".to_string());
                }
                let mut args = Vec::with_capacity(raw.leaves.len());
                for (index, leaf) in raw.leaves.iter().enumerate() {
                    args.push(leaf.to_expr(index)?);
                }
                PayloadExpr::and(args)
            }
        };
        expression
            .validate()
            .map_err(|error| format!("payload condition: {error}"))?;
        Ok(expression)
    }

    pub fn leaf_count(&self) -> usize {
        match self {
            Self::Frame(spec) => spec
                .conditions()
                .map(|conditions| {
                    conditions
                        .iter()
                        .map(|condition| condition.leaves.len())
                        .sum()
                })
                .unwrap_or(0),
            Self::Zigbee(spec) => spec.leaf_count(),
            Self::Raw(raw) => raw.leaves.len(),
        }
    }

    pub fn byte_count(&self) -> usize {
        match self {
            Self::Frame(spec) => spec
                .conditions()
                .map(|conditions| {
                    conditions
                        .iter()
                        .flat_map(|condition| &condition.leaves)
                        .map(|leaf| leaf.bytes.len())
                        .sum()
                })
                .unwrap_or(0),
            Self::Zigbee(spec) => spec.byte_count(),
            Self::Raw(raw) => raw.leaves.iter().map(RawLeafDraft::byte_count).sum(),
        }
    }

    pub fn max_leaf_bytes(&self) -> usize {
        match self {
            Self::Frame(spec) => spec
                .conditions()
                .map(|conditions| {
                    conditions
                        .iter()
                        .flat_map(|condition| &condition.leaves)
                        .map(|leaf| leaf.bytes.len())
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0),
            Self::Zigbee(spec) => spec.max_leaf_bytes(),
            Self::Raw(raw) => raw
                .leaves
                .iter()
                .map(|leaf| {
                    hodeco_codec::parse_hex(&leaf.bytes)
                        .map(|bytes| bytes.len())
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0),
        }
    }
}

impl RawLeafDraft {
    pub fn to_expr(&self, index: usize) -> Result<PayloadExpr, String> {
        let bytes = hodeco_codec::parse_hex(&self.bytes)
            .map_err(|error| format!("leaf {} bytes: {error}", index + 1))?;
        if bytes.is_empty() {
            return Err(format!("leaf {} needs bytes", index + 1));
        }
        let offset = if self.anywhere {
            PayloadOffset::anywhere()
        } else {
            let offset = self.offset.trim();
            let offset = if offset.is_empty() {
                0
            } else {
                offset.parse::<u32>().map_err(|_| {
                    format!("leaf {} offset must be a non-negative number", index + 1)
                })?
            };
            PayloadOffset::at(offset)
        };
        let mask = self.mask.trim();
        if mask.is_empty() {
            Ok(PayloadExpr::leaf(offset, bytes))
        } else {
            let mask = hodeco_codec::parse_hex(mask)
                .map_err(|error| format!("leaf {} mask: {error}", index + 1))?;
            Ok(PayloadExpr::masked_leaf(offset, bytes, mask))
        }
    }

    pub fn byte_count(&self) -> usize {
        hodeco_codec::parse_hex(&self.bytes)
            .map(|bytes| bytes.len())
            .unwrap_or(0)
            + hodeco_codec::parse_hex(&self.mask)
                .map(|mask| mask.len())
                .unwrap_or(0)
    }

    /// Effective preview using `??` for wildcard mask bytes.
    pub fn preview(&self) -> Result<String, String> {
        let bytes = hodeco_codec::parse_hex(&self.bytes).map_err(|error| error.to_string())?;
        if bytes.is_empty() {
            return Err("bytes are required".to_string());
        }
        let mask = hodeco_codec::parse_hex(&self.mask).map_err(|error| error.to_string())?;
        let length = if mask.is_empty() {
            bytes.len()
        } else {
            mask.len().min(bytes.len())
        };
        let preview = bytes
            .iter()
            .zip(mask.iter().chain(std::iter::repeat(&0xff)))
            .take(length)
            .map(|(byte, mask)| {
                if *mask == 0 {
                    "??".to_string()
                } else {
                    format!("{byte:02x}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let anchor = if self.anywhere {
            "anywhere".to_string()
        } else {
            format!("@{}", self.offset.trim())
        };
        Ok(format!("{anchor}: {preview}"))
    }

    pub fn warnings(&self) -> Vec<String> {
        let bytes = hodeco_codec::parse_hex(&self.bytes).unwrap_or_default();
        let mask = hodeco_codec::parse_hex(&self.mask).unwrap_or_default();
        let mut warnings = Vec::new();
        if !mask.is_empty() && mask.len() < bytes.len() {
            warnings.push(format!(
                "mask is shorter than bytes: only the first {} byte(s) are compared",
                mask.len()
            ));
        }
        if !mask.is_empty() && mask.len() > bytes.len() {
            warnings.push(format!(
                "mask is longer than bytes: {} extra mask byte(s) are ignored",
                mask.len() - bytes.len()
            ));
        }
        warnings
    }
}

#[derive(Debug, Clone, Default)]
pub struct FilterDraft {
    pub states: Vec<MessageState>,
    pub directions: Vec<MessageDirection>,
    pub protocols: Vec<String>,
    pub transports: Vec<String>,
    pub services: Vec<ServiceRef>,
    pub service_query: String,
    pub payload_conditions: Vec<PayloadConditionDraft>,
    pub error: Option<String>,
}

impl FilterDraft {
    fn from_filters(filters: &MessageFilters) -> Self {
        Self {
            states: filters.states.clone(),
            directions: filters.directions.clone(),
            protocols: filters.protocols.clone(),
            transports: filters.transports.clone(),
            services: filters.services.clone(),
            service_query: String::new(),
            payload_conditions: filters.payload_conditions.clone(),
            error: None,
        }
    }

    pub fn to_filters(&self) -> Result<MessageFilters, String> {
        let filters = MessageFilters {
            states: self.states.clone(),
            directions: self.directions.clone(),
            protocols: self.protocols.clone(),
            transports: self.transports.clone(),
            services: self.services.clone(),
            payload_conditions: self.payload_conditions.clone(),
        };
        filters.payload_expr()?;
        Ok(filters)
    }

    pub fn payload_expr(&self) -> Result<Option<PayloadExpr>, String> {
        let mut conditions = Vec::new();
        for condition in &self.payload_conditions {
            let expression = condition.to_expr()?;
            match expression {
                PayloadExpr::Group(group) if group.op == PayloadOp::Or && !group.neg => {
                    conditions.extend(group.args);
                }
                other => conditions.push(other),
            }
        }
        let expression = match conditions.len() {
            0 => None,
            1 => conditions.pop(),
            _ => Some(PayloadExpr::or(conditions)),
        };
        if let Some(expression) = &expression {
            expression
                .validate()
                .map_err(|error| format!("payload expression: {error}"))?;
        }
        Ok(expression)
    }

    pub fn leaf_count(&self) -> usize {
        self.payload_conditions
            .iter()
            .map(PayloadConditionDraft::leaf_count)
            .sum()
    }

    pub fn byte_count(&self) -> usize {
        self.payload_conditions
            .iter()
            .map(PayloadConditionDraft::byte_count)
            .sum()
    }

    pub fn exceeds_limits(&self) -> bool {
        self.leaf_count() > hodeco_contract::domain::MAX_PAYLOAD_LEAVES
            || self.byte_count() > hodeco_contract::domain::MAX_PAYLOAD_TOTAL_BYTES
            || self.payload_conditions.iter().any(|condition| {
                condition.max_leaf_bytes() > hodeco_contract::domain::MAX_PAYLOAD_LEAF_BYTES
            })
    }
}

impl MessageFilters {
    pub fn payload_expr(&self) -> Result<Option<PayloadExpr>, String> {
        let draft = FilterDraft {
            payload_conditions: self.payload_conditions.clone(),
            ..FilterDraft::default()
        };
        draft.payload_expr()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingNotification {
    New,
    StateChanged,
}

#[derive(Debug, Clone)]
pub struct MessagesUi {
    /// Bounded local list in oldest-first display order.
    pub messages: Vec<Message>,
    pub store_total: u64,
    pub loading: bool,
    pub loading_older: bool,
    pub loading_newer: bool,
    /// Bound pair returned by the last newest-first page. `None` means there
    /// are no older matching messages.
    pub older_next: Option<MessageTimeRange>,
    /// Bound pair returned by the last oldest-first page. `None` means there
    /// are no newer matching messages.
    pub newer_next: Option<MessageTimeRange>,
    pub selected: Option<Uuid>,
    pub selected_message: Option<Message>,
    pub view: MessageView,
    pub text_encoding: model::TextEncoding,
    pub search: String,
    pub search_mode: model::PayloadMode,
    pub search_error: Option<String>,
    pub search_status: Option<String>,
    pub searching: bool,
    pub search_generation: u64,
    pub search_direction: SearchDirection,
    pub search_from_selection: bool,
    pub list_generation: u64,
    pub error: Option<String>,
    pub filters: MessageFilters,
    pub draft: FilterDraft,
    pub options: MessageFilters,
    pub options_draft: FilterDraft,
    pub pending_notifications: HashMap<Uuid, PendingNotification>,
    pub notification_due_at: Option<Instant>,
    pub batch_in_flight: Vec<Uuid>,
    pub batch_in_flight_state_ids: HashSet<Uuid>,
    pub scroll_to_bottom: bool,
    pub scroll_to_selected: bool,
    /// Row to keep at the top after prepending an older page.
    pub scroll_anchor: Option<Uuid>,
    /// First row rendered in the last frame.
    pub visible_start: u64,
    /// The list is the keyboard navigation target. This is intentionally
    /// logical rather than egui's widget focus: context-panel controls may own
    /// widget focus while the arrow keys still drive the message list.
    pub list_keyboard: bool,
    pub at_bottom: bool,
}

impl Default for MessagesUi {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            store_total: 0,
            loading: true,
            loading_older: false,
            loading_newer: false,
            older_next: None,
            newer_next: None,
            selected: None,
            selected_message: None,
            view: MessageView::Summary,
            text_encoding: model::TextEncoding::Ascii,
            search: String::new(),
            search_mode: model::PayloadMode::Ascii,
            search_error: None,
            search_status: None,
            searching: false,
            search_generation: 0,
            search_direction: SearchDirection::Next,
            search_from_selection: false,
            list_generation: 0,
            error: None,
            filters: MessageFilters::default(),
            draft: FilterDraft::default(),
            options: MessageFilters::default(),
            options_draft: FilterDraft::default(),
            pending_notifications: HashMap::new(),
            notification_due_at: None,
            batch_in_flight: Vec::new(),
            batch_in_flight_state_ids: HashSet::new(),
            scroll_to_bottom: false,
            scroll_to_selected: false,
            scroll_anchor: None,
            visible_start: 0,
            list_keyboard: false,
            at_bottom: true,
        }
    }
}

impl MessagesUi {
    pub fn selected_message(&self) -> Option<&Message> {
        self.selected_message.as_ref()
    }

    pub fn message_at(&self, index: usize) -> Option<&Message> {
        self.messages.get(index)
    }

    pub fn message_by_id(&self, id: Uuid) -> Option<&Message> {
        self.messages.iter().find(|message| message.id == id)
    }

    pub fn selected_row(&self) -> Option<usize> {
        self.selected
            .and_then(|id| self.messages.iter().position(|message| message.id == id))
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.selected_message = None;
        self.scroll_to_selected = false;
    }

    pub fn invalidate_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.searching = false;
        self.search_status = None;
    }

    pub fn reset(&mut self) {
        let generation = self.list_generation.wrapping_add(1);
        let search_generation = self.search_generation.wrapping_add(1);
        let filters = self.filters.clone();
        let draft = self.draft.clone();
        let options = self.options.clone();
        let options_draft = self.options_draft.clone();
        let search = self.search.clone();
        let search_mode = self.search_mode;
        let view = self.view;
        let text_encoding = self.text_encoding;
        let list_keyboard = self.list_keyboard;
        *self = Self {
            list_generation: generation,
            search_generation,
            filters,
            draft,
            options,
            options_draft,
            search,
            search_mode,
            view,
            text_encoding,
            list_keyboard,
            ..Self::default()
        };
    }

    pub fn replace_page(
        &mut self,
        messages: Vec<Message>,
        next: Option<MessageTimeRange>,
        store_total: u64,
    ) {
        self.messages = sorted_unique(messages);
        self.older_next = next;
        self.newer_next = None;
        self.store_total = store_total;
        self.loading = false;
        self.loading_older = false;
        self.loading_newer = false;
    }

    pub fn prepend_older(
        &mut self,
        messages: Vec<Message>,
        next: Option<MessageTimeRange>,
        store_total: u64,
    ) {
        let anchor = self
            .message_at(self.visible_start as usize)
            .map(|message| message.id);
        let mut merged = messages;
        merged.extend(std::mem::take(&mut self.messages));
        self.messages = sorted_unique(merged);
        self.older_next = next;
        self.store_total = store_total;
        self.loading_older = false;
        self.trim_newest();
        self.scroll_anchor = anchor.filter(|id| self.message_by_id(*id).is_some());
    }

    pub fn append_newer(
        &mut self,
        messages: Vec<Message>,
        next: Option<MessageTimeRange>,
        store_total: u64,
    ) {
        let mut merged = std::mem::take(&mut self.messages);
        merged.extend(messages);
        self.messages = sorted_unique(merged);
        self.newer_next = next;
        self.store_total = store_total;
        self.loading_newer = false;
        self.trim_oldest();
    }

    pub fn upsert_messages(&mut self, messages: &[Message]) {
        for message in messages {
            if let Some(existing) = self
                .messages
                .iter_mut()
                .find(|existing| existing.id == message.id)
            {
                *existing = message.clone();
            } else {
                let index = self
                    .messages
                    .partition_point(|existing| existing.time <= message.time);
                self.messages.insert(index, message.clone());
            }
        }
        self.trim_oldest();
        if let Some(selected) = self.selected
            && let Some(message) = self.message_by_id(selected)
        {
            self.selected_message = Some(message.clone());
        }
    }

    pub fn remove_ids(&mut self, ids: &[Uuid]) {
        let removed: HashSet<Uuid> = ids.iter().copied().collect();
        self.messages
            .retain(|message| !removed.contains(&message.id));
        self.store_total = self.store_total.saturating_sub(removed.len() as u64);
        if self.selected.is_some_and(|id| removed.contains(&id)) {
            self.clear_selection();
        }
        self.refresh_paging_bounds();
    }

    fn trim_oldest(&mut self) {
        if self.messages.len() > model::MESSAGE_LIST_CAP {
            let excess = self.messages.len() - model::MESSAGE_LIST_CAP;
            self.messages.drain(0..excess);
            self.keep_selection_visible();
            self.refresh_older_next();
        }
    }

    fn trim_newest(&mut self) {
        if self.messages.len() > model::MESSAGE_LIST_CAP {
            self.messages.truncate(model::MESSAGE_LIST_CAP);
            self.keep_selection_visible();
            self.refresh_newer_next();
        }
    }

    /// Keeps a server-provided bound pair usable after local trimming or
    /// deletion by moving its known edge to the current local edge.
    fn refresh_paging_bounds(&mut self) {
        self.refresh_older_next();
        self.refresh_newer_next();
    }

    fn refresh_older_next(&mut self) {
        if let Some(previous) = &self.older_next {
            let from = previous.from;
            self.older_next = self.messages.first().map(|message| MessageTimeRange {
                from,
                until: Some(message.time),
            });
        }
    }

    fn refresh_newer_next(&mut self) {
        if let Some(previous) = &self.newer_next {
            let until = previous.until;
            self.newer_next = self.messages.last().and_then(|message| {
                message
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
                    .map(|from| MessageTimeRange {
                        from: Some(from),
                        until,
                    })
            });
        }
    }

    fn keep_selection_visible(&mut self) {
        if let Some(selected) = self.selected
            && self.message_by_id(selected).is_none()
        {
            self.clear_selection();
        }
    }
}

fn sorted_unique(mut messages: Vec<Message>) -> Vec<Message> {
    messages.sort_by_key(|message| message.time);
    messages.dedup_by_key(|message| message.id);
    messages
}

#[derive(Debug, Clone)]
pub struct DiscoveryHistoryEntry {
    pub info: DiscoveryInfo,
    pub seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryUi {
    pub protocol: String,
    pub transport: String,
    pub editor: Option<crate::ui::params::ParamEditor>,
    pub editor_key: Option<(String, String)>,
    pub current: Option<DiscoveryInfo>,
    pub history: Vec<DiscoveryHistoryEntry>,
    /// History entry being shown; `None` follows `current` and live updates.
    pub viewing: Option<Uuid>,
    pub starting: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ConsoleOutcome {
    Pending,
    Success(Value),
    Error(ApiError),
    Timeout,
    LocalError(String),
}

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub id: u64,
    pub time: DateTime<Utc>,
    pub method: String,
    pub params: String,
    pub outcome: ConsoleOutcome,
}

#[derive(Debug, Clone)]
pub struct ConsoleUi {
    pub method: String,
    pub params: String,
    pub entries: Vec<ConsoleEntry>,
    pub pending: bool,
    pub next_id: u64,
    /// Newly added history entry that should be expanded and scrolled to.
    pub expand_id: Option<u64>,
    pub error: Option<String>,
}

impl Default for ConsoleUi {
    fn default() -> Self {
        Self {
            method: "server.info".to_string(),
            params: "{}".to_string(),
            entries: Vec::new(),
            pending: false,
            next_id: 1,
            expand_id: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToastEntry {
    pub toast: Toast,
    pub created: Instant,
}

// ---------------------------------------------------------------------------
// Dialog state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConnectDialog {
    pub endpoint: String,
    pub demo: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AddServiceDialog {
    pub alias: String,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    pub editor: Option<crate::ui::params::ParamEditor>,
    pub editor_key: Option<(String, String)>,
    pub error: Option<String>,
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub struct EditAliasDialog {
    pub service: ServiceRef,
    pub label: String,
    pub alias: String,
    pub error: Option<String>,
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub struct ServiceParamsDialog {
    pub service: ServiceInfo,
    pub editor: Option<crate::ui::params::ParamEditor>,
    pub editor_key: Option<(String, String)>,
    pub pending: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageDialog {
    pub id: Uuid,
    pub message: Option<Message>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterDialogMode {
    Filters,
    SearchOptions,
}

#[derive(Debug, Clone)]
pub struct ConditionEditor {
    /// `None` while adding a new condition.
    pub index: Option<usize>,
    pub draft: PayloadConditionDraft,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageFiltersDialog {
    pub mode: FilterDialogMode,
    pub editor: Option<ConditionEditor>,
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Connect(ConnectDialog),
    AddService(AddServiceDialog),
    EditAlias(EditAliasDialog),
    ConfirmRemove(ServiceInfo),
    ConfirmRestart,
    ServiceStatus { service: ServiceRef },
    ServiceParams(ServiceParamsDialog),
    ConfigDirty,
    Message(MessageDialog),
    MessageFilters(MessageFiltersDialog),
    Builder(Box<BuilderDialog>),
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub(crate) gateway: Gateway,
    pub(crate) wake: Wake,
    pub(crate) theme: UiTheme,
    applied_theme: Option<UiTheme>,
    pub(crate) tab: Tab,
    pub(crate) services_tab: ServicesTab,
    pub(crate) tasks_tab: TasksTab,

    pub(crate) server: Option<ServerInfoResult>,
    pub(crate) protocols: Vec<ProtocolInfo>,
    pub(crate) transports: Vec<TransportInfo>,
    pub(crate) services: Vec<ServiceInfo>,
    pub(crate) config: Option<Config>,

    pub(crate) messages: MessagesUi,
    pub(crate) discovery: DiscoveryUi,
    pub(crate) tasks: views::tasks::TasksUi,
    pub(crate) schedules: views::schedules::SchedulesUi,
    pub(crate) commands: views::commands::CommandsUi,
    pub(crate) runs: views::runs::RunsUi,
    pub(crate) console: ConsoleUi,
    pub(crate) config_ui: views::config_ui::ConfigUi,
    pub(crate) protocol_params: HashMap<(String, String), Vec<ParamDef>>,
    pub(crate) protocol_params_requested: HashSet<(String, String)>,
    pub(crate) protocol_params_errors: HashMap<(String, String), String>,

    pub(crate) dialog: Option<Dialog>,
    pub(crate) pending_tab: Option<Tab>,
    pub(crate) toasts: Vec<ToastEntry>,

    pub(crate) last_error: Option<String>,
    pub(crate) last_sync: Option<DateTime<Utc>>,
    pub(crate) protocol_mismatch: bool,
    pub(crate) stale: bool,
    pub(crate) restarting: bool,

    pub(crate) now: DateTime<Utc>,
    discovery_poll_at: Option<Instant>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        let theme = UiTheme::Dark;
        theme.apply(&cc.egui_ctx);
        #[cfg(not(target_arch = "wasm32"))]
        crate::theme::install_native_fallback_fonts(&cc.egui_ctx);

        let ctx = cc.egui_ctx.clone();
        let wake: Wake = Arc::new(move || ctx.request_repaint());

        let mut gateway = Gateway::new(startup.endpoint.clone(), startup.demo);
        gateway.start(Instant::now());

        Self {
            gateway,
            wake,
            theme,
            applied_theme: Some(theme),
            tab: Tab::Services,
            services_tab: ServicesTab::Services,
            tasks_tab: TasksTab::Tasks,
            server: None,
            protocols: Vec::new(),
            transports: Vec::new(),
            services: Vec::new(),
            config: None,
            messages: MessagesUi::default(),
            discovery: DiscoveryUi::default(),
            tasks: views::tasks::TasksUi::default(),
            schedules: views::schedules::SchedulesUi::default(),
            commands: views::commands::CommandsUi::default(),
            runs: views::runs::RunsUi::default(),
            console: ConsoleUi::default(),
            config_ui: views::config_ui::ConfigUi::default(),
            protocol_params: HashMap::new(),
            protocol_params_requested: HashSet::new(),
            protocol_params_errors: HashMap::new(),
            dialog: None,
            pending_tab: None,
            toasts: Vec::new(),
            last_error: None,
            last_sync: None,
            protocol_mismatch: false,
            stale: false,
            restarting: false,
            now: Utc::now(),
            discovery_poll_at: None,
        }
    }

    // -- small helpers -----------------------------------------------------

    pub(crate) fn palette(&self) -> crate::theme::Palette {
        palette(self.theme)
    }

    pub(crate) fn toast(&mut self, tone: Tone, text: impl Into<String>) {
        self.toasts.push(ToastEntry {
            toast: Toast {
                text: text.into(),
                tone,
            },
            created: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub(crate) fn rpc(&mut self, tag: RequestTag, method: Method, params: Value) -> bool {
        match self.gateway.request(tag, method, params, Instant::now()) {
            Ok(()) => true,
            Err(error) => {
                self.toast(Tone::Error, error);
                false
            }
        }
    }

    pub(crate) fn open_connect_dialog(&mut self) {
        self.dialog = Some(Dialog::Connect(ConnectDialog {
            endpoint: if self.gateway.is_demo() {
                String::new()
            } else {
                self.gateway.endpoint().to_string()
            },
            demo: self.gateway.is_demo(),
            error: None,
        }));
    }

    pub(crate) fn open_add_service(
        &mut self,
        protocol: impl Into<String>,
        transport: impl Into<String>,
        entry: impl Into<String>,
    ) {
        self.dialog = Some(Dialog::AddService(AddServiceDialog {
            alias: String::new(),
            protocol: protocol.into(),
            transport: transport.into(),
            entry: entry.into(),
            editor: None,
            editor_key: None,
            error: None,
            pending: false,
        }));
    }

    pub(crate) fn open_builder(&mut self, service: Option<ServiceRef>) {
        self.dialog = Some(Dialog::Builder(Box::new(new_builder(
            service,
            &self.services,
            &self.protocols,
        ))));
    }

    pub(crate) fn open_task_message_builder(
        &mut self,
        service: Option<ServiceRef>,
        action_index: usize,
        payload: Base64Bytes,
        ttl: Option<HumanDuration>,
    ) {
        self.dialog = Some(Dialog::Builder(Box::new(new_task_message_builder(
            service,
            &self.services,
            &self.protocols,
            action_index,
            &payload,
            ttl,
        ))));
    }

    pub(crate) fn set_task_action_message(
        &mut self,
        action_index: usize,
        service: ServiceRef,
        payload: Base64Bytes,
        ttl: Option<HumanDuration>,
    ) -> bool {
        let Some(editor) = self.tasks.editor.as_mut() else {
            return false;
        };
        let Some(action) = editor.draft.actions.get_mut(action_index) else {
            return false;
        };
        let ActionKind::Message(message) = &mut action.kind else {
            return false;
        };
        message.message.service = service;
        message.message.payload = payload;
        message.message.ttl = ttl;
        true
    }

    pub(crate) fn open_message(&mut self, id: Uuid) {
        let message = self.messages.message_by_id(id).cloned().or_else(|| {
            self.messages
                .selected_message()
                .filter(|message| message.id == id)
                .cloned()
        });
        let loading = message.is_none();
        self.dialog = Some(Dialog::Message(MessageDialog {
            id,
            message,
            loading,
            error: None,
        }));
        if loading {
            self.fetch_message(id);
        }
    }

    pub(crate) fn fetch_message(&mut self, id: Uuid) {
        if let Some(Dialog::Message(dialog)) = &mut self.dialog {
            dialog.loading = true;
            dialog.error = None;
        }
        if !self.rpc(
            RequestTag::MessageGet { id },
            Method::MessagesGet,
            json!({"id": id}),
        ) && let Some(Dialog::Message(dialog)) = &mut self.dialog
        {
            dialog.loading = false;
            dialog.error = Some("not connected to the gateway".to_string());
        }
    }

    fn update_message_dialog(&mut self, message: &Message) {
        let Some(Dialog::Message(dialog)) = &mut self.dialog else {
            return;
        };
        if dialog.id == message.id {
            dialog.message = Some(message.clone());
            dialog.loading = false;
            dialog.error = None;
        }
    }

    fn finish_message_fetch(&mut self, id: Uuid, result: Result<Message, String>) {
        let Some(Dialog::Message(dialog)) = &mut self.dialog else {
            return;
        };
        if dialog.id != id {
            return;
        }
        dialog.loading = false;
        match result {
            Ok(message) => {
                dialog.message = Some(message);
                dialog.error = None;
            }
            Err(error) => dialog.error = Some(error),
        }
    }

    pub(crate) fn open_message_filters(&mut self) {
        self.messages.draft = FilterDraft::from_filters(&self.messages.filters);
        self.dialog = Some(Dialog::MessageFilters(MessageFiltersDialog {
            mode: FilterDialogMode::Filters,
            editor: None,
        }));
    }

    pub(crate) fn open_search_options(&mut self) {
        self.messages.options_draft = FilterDraft::from_filters(&self.messages.options);
        self.dialog = Some(Dialog::MessageFilters(MessageFiltersDialog {
            mode: FilterDialogMode::SearchOptions,
            editor: None,
        }));
    }

    pub(crate) fn open_service_status(&mut self, service: &ServiceInfo) {
        self.dialog = Some(Dialog::ServiceStatus {
            service: model::service_ref(service),
        });
    }

    pub(crate) fn open_service_params(&mut self, service: &ServiceInfo) {
        self.ensure_protocol_params(&service.protocol, &service.transport);
        self.dialog = Some(Dialog::ServiceParams(ServiceParamsDialog {
            service: service.clone(),
            editor: None,
            editor_key: None,
            pending: false,
            error: None,
        }));
    }

    pub(crate) fn open_edit_alias(&mut self, service: &ServiceInfo) {
        self.dialog = Some(Dialog::EditAlias(EditAliasDialog {
            service: model::service_ref(service),
            label: model::service_key_label(service),
            alias: service.alias.clone().unwrap_or_default(),
            error: None,
            pending: false,
        }));
    }

    // -- actions -----------------------------------------------------------

    pub(crate) fn refresh_services(&mut self) {
        self.rpc(RequestTag::ServiceList, Method::ServicesList, json!({}));
    }

    pub(crate) fn refresh_config(&mut self) {
        self.rpc(RequestTag::Config, Method::ConfigGet, json!({}));
    }

    pub(crate) fn save_config_draft(&mut self) {
        if self.config_ui.saving {
            return;
        }
        let config = match self.config_ui.build() {
            Ok(config) => config,
            Err(error) => {
                self.config_ui.error = Some(error);
                return;
            }
        };
        self.config_ui.error = None;
        self.config_ui.saving = true;
        if !self.rpc(
            RequestTag::ConfigUpdate,
            Method::ConfigUpdate,
            json!({"config": config}),
        ) {
            self.config_ui.saving = false;
            self.config_ui.error = Some("the gateway is not connected".to_string());
        }
    }

    pub(crate) fn discard_config_draft(&mut self) {
        match self.config.clone() {
            Some(config) => self.config_ui.load(&config),
            None => {
                self.config_ui.draft = None;
                self.config_ui.dirty = false;
            }
        }
        self.config_ui.saving = false;
        self.config_ui.saved = false;
        self.config_ui.restart_required.clear();
        self.config_ui.error = None;
    }

    // -- automation actions ------------------------------------------------

    pub(crate) fn request_tasks_tab(&mut self, tab: TasksTab) {
        if tab == self.tasks_tab {
            return;
        }
        if self.tasks_tab == TasksTab::Tasks && self.tasks.editor_dirty() {
            if let Some(editor) = self.tasks.editor.as_mut() {
                editor.error =
                    Some("Save or discard the task changes before leaving the editor.".to_string());
            }
            return;
        }
        if self.tasks_tab == TasksTab::Schedules
            && self
                .schedules
                .editor
                .as_ref()
                .is_some_and(|editor| editor.is_dirty())
        {
            if let Some(editor) = self.schedules.editor.as_mut() {
                editor.error = Some(
                    "Save or discard the schedule changes before leaving the editor.".to_string(),
                );
            }
            return;
        }
        self.tasks_tab = tab;
        if tab == TasksTab::Runs && self.runs.runs.is_empty() && !self.runs.loading {
            self.refresh_runs();
        }
    }

    pub(crate) fn refresh_tasks(&mut self) {
        if self.rpc(RequestTag::TasksList, Method::TasksList, json!({})) {
            self.tasks.loading = true;
            self.tasks.error = None;
        }
    }

    pub(crate) fn refresh_schedules(&mut self) {
        if self.rpc(RequestTag::SchedulesList, Method::SchedulesList, json!({})) {
            self.schedules.loading = true;
            self.schedules.error = None;
        }
    }

    pub(crate) fn refresh_commands(&mut self) {
        if self.rpc(RequestTag::CommandsList, Method::CommandsList, json!({})) {
            self.commands.loading = true;
            self.commands.error = None;
        }
    }

    pub(crate) fn open_task_editor(&mut self, task: Option<TaskDefinition>) {
        let (task, is_new) = match task {
            Some(task) => (task, false),
            None => {
                let mut task = TaskDefinition {
                    name: "New task".to_string(),
                    ..TaskDefinition::default()
                };
                task.id = hodeco_contract::utils::id_from_name("task", &task.name);
                (task, true)
            }
        };
        self.tasks.editor = Some(views::tasks::TaskEditor::new(task, is_new));
    }

    pub(crate) fn save_task(&mut self, task: TaskDefinition, is_new: bool) -> bool {
        let params = if is_new {
            json!({ "task": task })
        } else {
            json!({ "taskId": task.id, "task": task })
        };
        let method = if is_new {
            Method::TasksAdd
        } else {
            Method::TasksUpdate
        };
        let tag = if is_new {
            RequestTag::TaskAdd
        } else {
            RequestTag::TaskUpdate
        };
        self.rpc(tag, method, params)
    }

    pub(crate) fn set_task_enabled(&mut self, task: &TaskDefinition, enabled: bool) {
        let mut updated = task.clone();
        updated.enabled = enabled;
        if self.rpc(
            RequestTag::TaskUpdate,
            Method::TasksUpdate,
            json!({ "taskId": updated.id, "task": updated }),
        ) {
            self.refresh_tasks();
        }
    }

    pub(crate) fn delete_task(&mut self, task_id: &str) {
        if self.rpc(
            RequestTag::TaskRemove {
                task_id: task_id.to_string(),
            },
            Method::TasksRemove,
            json!({ "taskId": task_id }),
        ) {
            self.tasks.confirm_delete = None;
        }
    }

    pub(crate) fn fire_task(&mut self, task: &TaskDefinition) {
        self.fire_task_with_payload(&task.id, (None, serde_json::Map::new()));
    }

    pub(crate) fn fire_task_with_payload(
        &mut self,
        task_id: &str,
        (payload, metadata): (Option<Base64Bytes>, serde_json::Map<String, Value>),
    ) {
        self.tasks.firing = true;
        let params = json!({
            "taskId": task_id,
            "payload": payload,
            "metadata": metadata,
        });
        if !self.rpc(RequestTag::TaskFire, Method::TasksFire, params) {
            self.tasks.firing = false;
        }
    }

    pub(crate) fn cancel_task_run(&mut self, task_id: &str, run_id: Uuid) {
        if !self.runs.cancelling.insert(run_id) {
            return;
        }
        if !self.rpc(
            RequestTag::TaskCancel {
                task_id: task_id.to_string(),
                run_id,
            },
            Method::TasksCancelRun,
            json!({ "taskId": task_id, "runId": run_id }),
        ) {
            self.runs.cancelling.remove(&run_id);
        }
    }

    pub(crate) fn purge_task_history(&mut self, task_id: &str) {
        self.rpc(
            RequestTag::TaskPurgeHistory {
                task_id: task_id.to_string(),
            },
            Method::TasksPurgeHistory,
            json!({ "taskId": task_id }),
        );
    }

    pub(crate) fn open_schedule_editor(&mut self, schedule: Option<ScheduleDefinition>) {
        let (schedule, is_new) = match schedule {
            Some(schedule) => (schedule, false),
            None => (
                ScheduleDefinition {
                    name: "New schedule".to_string(),
                    at: hodeco_contract::domain::OneOrMany::One(
                        hodeco_contract::domain::AtSpec::Text("09:00".to_string()),
                    ),
                    ..ScheduleDefinition::default()
                },
                true,
            ),
        };
        let fallback_timezone = self
            .schedules
            .defaults_timezone
            .clone()
            .or_else(|| self.schedules.system_timezone.clone());
        self.schedules.editor = Some(views::schedules::ScheduleEditor::new(
            schedule,
            is_new,
            fallback_timezone,
        ));
    }

    pub(crate) fn save_schedule(&mut self, schedule: ScheduleDefinition, is_new: bool) -> bool {
        let params = if is_new {
            json!({ "schedule": schedule })
        } else {
            json!({ "scheduleId": schedule.id, "schedule": schedule })
        };
        let method = if is_new {
            Method::SchedulesAdd
        } else {
            Method::SchedulesUpdate
        };
        let tag = if is_new {
            RequestTag::ScheduleAdd
        } else {
            RequestTag::ScheduleUpdate
        };
        self.rpc(tag, method, params)
    }

    pub(crate) fn delete_schedule(&mut self, schedule_id: &str) {
        if self.rpc(
            RequestTag::ScheduleRemove {
                schedule_id: schedule_id.to_string(),
            },
            Method::SchedulesRemove,
            json!({ "scheduleId": schedule_id }),
        ) {
            self.schedules.confirm_delete = None;
        }
    }

    pub(crate) fn open_schedule_defaults(&mut self) {
        self.schedules.defaults_editor = Some(views::schedules::ScheduleDefaultsEditor::new(
            self.schedules.defaults_location,
            self.schedules.defaults_timezone.clone(),
        ));
    }

    pub(crate) fn open_location_dialog(&mut self, target: views::schedules::LocationTarget) {
        let (location, timezone) = match target {
            views::schedules::LocationTarget::Defaults => {
                let editor = self.schedules.defaults_editor.as_ref();
                let location = editor
                    .and_then(|editor| {
                        views::schedules::parse_location(&editor.location)
                            .ok()
                            .flatten()
                    })
                    .or(self.schedules.defaults_location);
                let timezone = editor
                    .filter(|editor| editor.timezone_custom)
                    .map(|editor| editor.timezone.trim().to_string())
                    .unwrap_or_default();
                (location, timezone)
            }
            views::schedules::LocationTarget::Schedule => {
                let editor = self.schedules.editor.as_ref();
                let location = editor
                    .and_then(|editor| {
                        views::schedules::parse_location(&editor.location)
                            .ok()
                            .flatten()
                    })
                    .or(self.schedules.defaults_location);
                let timezone = editor
                    .filter(|editor| editor.timezone_custom)
                    .map(|editor| editor.timezone.trim().to_string())
                    .filter(|timezone| !timezone.is_empty())
                    .or_else(|| self.schedules.defaults_timezone.clone())
                    .unwrap_or_default();
                (location, timezone)
            }
        };
        self.schedules.location_dialog = Some(views::schedules::LocationDialog::new(
            target, location, timezone,
        ));
    }

    pub(crate) fn preview_solar(
        &mut self,
        location: Location,
        date: NaiveDate,
        timezone: String,
        generation: u64,
    ) -> bool {
        let mut params = json!({
            "location": location,
            "date": date.format("%Y-%m-%d").to_string(),
        });
        if !timezone.trim().is_empty() {
            params["timezone"] = json!(timezone.trim());
        }
        self.rpc(
            RequestTag::SolarPreview { generation },
            Method::SchedulesSolarPreview,
            params,
        )
    }

    pub(crate) fn save_schedule_defaults(
        &mut self,
        location: Option<Location>,
        timezone: Option<String>,
    ) -> bool {
        let mut params = serde_json::Map::new();
        if let Some(location) = location
            && let Ok(value) = serde_json::to_value(location)
        {
            params.insert("location".to_string(), value);
        }
        if let Some(timezone) = timezone {
            params.insert("timezone".to_string(), Value::String(timezone));
        }
        self.rpc(
            RequestTag::ScheduleSetDefaults,
            Method::SchedulesSetDefaults,
            Value::Object(params),
        )
    }

    pub(crate) fn preview_schedule(
        &mut self,
        schedule: &ScheduleDefinition,
        generation: u64,
    ) -> bool {
        let params = json!({ "schedule": schedule, "limit": 20 });
        self.rpc(
            RequestTag::SchedulePreview { generation },
            Method::SchedulesPreview,
            params,
        )
    }

    pub(crate) fn open_command_editor(&mut self, command: Option<CommandDefinition>) {
        let (command, is_new, used_by) = match command {
            Some(command) => {
                let used_by = self
                    .commands
                    .commands
                    .iter()
                    .find(|info| info.command.id == command.id)
                    .map(|info| info.used_by.clone())
                    .unwrap_or_default();
                (command, false, used_by)
            }
            None => (
                CommandDefinition {
                    name: "New command".to_string(),
                    ..CommandDefinition::default()
                },
                true,
                Vec::new(),
            ),
        };
        self.commands.editor = Some(views::commands::CommandEditor::new(
            command, is_new, used_by,
        ));
    }

    pub(crate) fn save_command(&mut self, command: CommandDefinition, is_new: bool) -> bool {
        let params = if is_new {
            json!({ "command": command })
        } else {
            json!({ "commandId": command.id, "command": command })
        };
        let method = if is_new {
            Method::CommandsAdd
        } else {
            Method::CommandsUpdate
        };
        let tag = if is_new {
            RequestTag::CommandAdd
        } else {
            RequestTag::CommandUpdate
        };
        self.rpc(tag, method, params)
    }

    pub(crate) fn delete_command(&mut self, command_id: &str) {
        if self.rpc(
            RequestTag::CommandRemove {
                command_id: command_id.to_string(),
            },
            Method::CommandsRemove,
            json!({ "commandId": command_id }),
        ) {
            self.commands.confirm_delete = None;
        }
    }

    pub(crate) fn refresh_runs(&mut self) {
        self.request_run_feed(RunPageKind::Refresh);
    }

    pub(crate) fn load_older_runs(&mut self) {
        self.request_run_feed(RunPageKind::Older);
    }

    pub(crate) fn load_newer_runs(&mut self) {
        self.request_run_feed(RunPageKind::Newer);
    }

    fn request_run_feed(&mut self, kind: RunPageKind) {
        if kind != RunPageKind::Refresh && self.runs.loading {
            return;
        }
        if kind == RunPageKind::Older
            && (self.runs.loading_older
                || self.runs.loading_newer
                || self.runs.older_next.is_none())
        {
            return;
        }
        if kind == RunPageKind::Newer
            && (self.runs.loading_newer
                || self.runs.loading_older
                || self.runs.newer_next.is_none())
        {
            return;
        }
        self.runs.generation = self.runs.generation.wrapping_add(1);
        let generation = self.runs.generation;
        let params = match views::runs::run_feed_params(self, kind) {
            Ok(params) => params,
            Err(error) => {
                self.runs.error = Some(error);
                return;
            }
        };
        let params = serde_json::to_value(params).unwrap_or_else(|_| json!({}));
        if self.rpc(
            RequestTag::TaskRunFeed { generation, kind },
            Method::TasksRunFeed,
            params,
        ) {
            match kind {
                RunPageKind::Refresh => {
                    self.runs.loading = true;
                    self.runs.older_next = None;
                    self.runs.newer_next = None;
                    self.runs.scroll_to_bottom =
                        self.runs.selected.is_none() && self.runs.selected_running.is_none();
                }
                RunPageKind::Older => self.runs.loading_older = true,
                RunPageKind::Newer => self.runs.loading_newer = true,
            }
            self.runs.error = None;
        } else {
            self.runs.loading = false;
            self.runs.loading_older = false;
            self.runs.loading_newer = false;
            self.runs.error = Some("The gateway is not connected.".to_string());
        }
    }

    fn apply_run_feed_page(&mut self, kind: RunPageKind, page: TasksRunFeedResult) {
        for run in &page.runs {
            self.runs.running.retain(|running| running.id != run.id);
        }
        self.runs.feed_rows = page.feed.rows;
        self.runs.feed_bytes = page.feed.bytes;
        self.runs.feed_max_rows = page.feed.max_rows;
        self.runs.feed_max_size = page.feed.max_size;
        let cap = views::runs::RUN_LIST_CAP.min(self.runs.feed_max_rows.max(1) as usize);

        match kind {
            RunPageKind::Refresh => {
                self.runs.runs = views::runs::sorted_runs(page.runs);
                self.runs.older_next = page.next;
                self.runs.newer_next = None;
                self.runs.scroll_anchor = None;
                self.runs.scroll_to_bottom =
                    self.runs.selected.is_none() && self.runs.selected_running.is_none();
                self.runs.at_bottom = true;
            }
            RunPageKind::Older => {
                self.runs.scroll_anchor = self.runs.runs.first().map(|run| run.id);
                let mut merged = page.runs;
                merged.extend(std::mem::take(&mut self.runs.runs));
                self.runs.runs = views::runs::sorted_runs(merged);
                self.runs.older_next = page.next;
                if self.runs.runs.len() > cap {
                    let excess = self.runs.runs.len() - cap;
                    let first_removed = self.runs.runs[self.runs.runs.len() - excess].time;
                    self.runs.runs.truncate(cap);
                    self.runs.newer_next = Some(MessageTimeRange {
                        from: Some(first_removed),
                        until: self.runs.newer_next.as_ref().and_then(|next| next.until),
                    });
                }
            }
            RunPageKind::Newer => {
                let mut merged = std::mem::take(&mut self.runs.runs);
                merged.extend(page.runs);
                self.runs.runs = views::runs::sorted_runs(merged);
                self.runs.newer_next = page.next;
                if self.runs.runs.len() > cap {
                    let excess = self.runs.runs.len() - cap;
                    let until = self.runs.runs[excess - 1]
                        .time
                        .checked_add_signed(chrono::TimeDelta::nanoseconds(1));
                    self.runs.runs.drain(0..excess);
                    self.runs.older_next = Some(MessageTimeRange {
                        from: self.runs.older_next.as_ref().and_then(|next| next.from),
                        until,
                    });
                }
                if self.runs.selected.is_none() {
                    self.runs.scroll_to_bottom = true;
                }
            }
        }

        if !self.runs.pending_feed.is_empty() || !self.runs.pending_dropped.is_empty() {
            let mut merged = std::mem::take(&mut self.runs.runs);
            merged.extend(std::mem::take(&mut self.runs.pending_feed));
            self.runs.runs = views::runs::sorted_runs(merged);
            if !self.runs.pending_dropped.is_empty() {
                let dropped = std::mem::take(&mut self.runs.pending_dropped);
                self.runs.runs.retain(|run| !dropped.contains(&run.id));
            }
            if self.runs.runs.len() > cap {
                let excess = self.runs.runs.len() - cap;
                let until = self.runs.runs[excess - 1]
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1));
                self.runs.runs.drain(0..excess);
                self.runs.older_next = Some(MessageTimeRange {
                    from: self.runs.older_next.as_ref().and_then(|next| next.from),
                    until,
                });
            }
        }

        self.runs.selected_index = self.runs.selected.as_ref().and_then(|(task, run)| {
            self.runs
                .runs
                .iter()
                .position(|candidate| candidate.task_id == *task && candidate.id == *run)
        });
        if self.runs.selected.is_some() && self.runs.selected_index.is_none() {
            views::runs::clear_run_selection(self);
        }
        self.runs.error = None;
    }

    pub(crate) fn select_run(&mut self, task_id: &str, run_id: Uuid) {
        self.runs.selected = Some((task_id.to_string(), run_id));
        self.runs.selected_running = None;
        self.runs.selected_index = self
            .runs
            .runs
            .iter()
            .position(|run| run.task_id == task_id && run.id == run_id);
        self.runs.detail = None;
        self.runs.detail_loading = true;
        self.runs.detail_error = None;
        if !self.rpc(
            RequestTag::TaskRunGet {
                task_id: task_id.to_string(),
                run_id,
            },
            Method::TasksGetRun,
            json!({ "taskId": task_id, "runId": run_id }),
        ) {
            self.runs.detail_loading = false;
            self.runs.detail_error = Some("The gateway is not connected.".to_string());
        }
    }

    pub(crate) fn open_task_history(&mut self, task_id: &str) {
        let name = self
            .tasks
            .tasks
            .iter()
            .find(|info| info.task.id == task_id)
            .map(|info| info.task.name.clone())
            .unwrap_or_else(|| task_id.to_string());
        let mut history = views::task_history::TaskHistoryUi::new(task_id.to_string(), name);
        views::task_history::request_page(self, &mut history, RunPageKind::Refresh);
        self.tasks.history = Some(history);
    }

    pub(crate) fn request_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        if self.tab == Tab::Configuration {
            let dirty = match (&self.config, self.config_ui.draft.as_mut()) {
                (Some(config), Some(draft)) => draft.differs_from(config),
                _ => false,
            };
            self.config_ui.dirty = dirty;
            if dirty {
                self.pending_tab = Some(tab);
                self.dialog = Some(Dialog::ConfigDirty);
                return;
            }
        }
        if self.tab == Tab::Tasks && tab != Tab::Tasks {
            if self.tasks_tab == TasksTab::Tasks && self.tasks.editor_dirty() {
                if let Some(editor) = self.tasks.editor.as_mut() {
                    editor.error = Some(
                        "Save or discard the task changes before leaving the editor.".to_string(),
                    );
                }
                return;
            }
            if self.tasks_tab == TasksTab::Schedules
                && self
                    .schedules
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.is_dirty())
            {
                if let Some(editor) = self.schedules.editor.as_mut() {
                    editor.error = Some(
                        "Save or discard the schedule changes before leaving the editor."
                            .to_string(),
                    );
                }
                return;
            }
        }
        if self.tab == Tab::Messages && tab != Tab::Messages {
            self.messages.list_keyboard = false;
        }
        self.tab = tab;
    }

    pub(crate) fn ensure_protocol_params(&mut self, protocol: &str, transport: &str) {
        let key = (protocol.to_string(), transport.to_string());
        if self.protocol_params.contains_key(&key)
            || self.protocol_params_requested.contains(&key)
            || self.protocol_params_errors.contains_key(&key)
        {
            return;
        }
        let tag = RequestTag::ProtocolParams {
            protocol: key.0.clone(),
            transport: key.1.clone(),
        };
        if self.rpc(
            tag,
            Method::ProtocolsParams,
            json!({"protocol": protocol, "transport": transport}),
        ) {
            self.protocol_params_requested.insert(key);
        }
    }

    pub(crate) fn refresh_server(&mut self) {
        self.rpc(RequestTag::ServerInfo, Method::ServerInfo, json!({}));
        self.rpc(RequestTag::ProtocolList, Method::ProtocolsList, json!({}));
        self.rpc(RequestTag::TransportList, Method::TransportsList, json!({}));
    }

    pub(crate) fn refresh_messages(&mut self) {
        self.request_message_page(MessagePageKind::Refresh);
    }

    pub(crate) fn message_search_text_changed(&mut self) {
        self.messages.search_error = None;
        self.messages.invalidate_search();
    }

    pub(crate) fn message_search_mode_changed(&mut self) {
        self.messages.search_error = None;
        self.messages.invalidate_search();
    }

    /// Invalidates the local list and loads its first filtered page.
    fn reload_message_list(&mut self) {
        self.messages.list_generation = self.messages.list_generation.wrapping_add(1);
        self.messages.messages.clear();
        self.messages.older_next = None;
        self.messages.newer_next = None;
        self.messages.loading_older = false;
        self.messages.loading_newer = false;
        self.messages.loading = true;
        self.messages.error = None;
        self.messages.clear_selection();
        self.messages.invalidate_search();
        let in_flight = std::mem::take(&mut self.messages.batch_in_flight);
        let in_flight_state = std::mem::take(&mut self.messages.batch_in_flight_state_ids);
        for id in in_flight {
            let kind = if in_flight_state.contains(&id) {
                PendingNotification::StateChanged
            } else {
                PendingNotification::New
            };
            self.messages
                .pending_notifications
                .entry(id)
                .or_insert(kind);
        }
        self.messages.notification_due_at = (!self.messages.pending_notifications.is_empty())
            .then(|| Instant::now() + Duration::from_millis(200));
        self.request_message_page(MessagePageKind::Refresh);
    }

    #[allow(clippy::too_many_arguments)]
    fn message_list_params(
        &self,
        filters: &MessageFilters,
        payload: Option<&PayloadExpr>,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        order: MessageOrder,
        limit: u32,
        ids: &[Uuid],
    ) -> Result<Value, String> {
        let mut params = serde_json::Map::new();
        if let Some(from) = from {
            params.insert("from".to_string(), json!(from));
        }
        if let Some(until) = until {
            params.insert("until".to_string(), json!(until));
        }
        params.insert("order".to_string(), json!(order));
        params.insert("limit".to_string(), json!(limit));
        if !filters.states.is_empty() {
            params.insert("states".to_string(), json!(filters.states));
        }
        if !filters.directions.is_empty() {
            params.insert("directions".to_string(), json!(filters.directions));
        }
        if !filters.protocols.is_empty() {
            params.insert("protocols".to_string(), json!(filters.protocols));
        }
        if !filters.transports.is_empty() {
            params.insert("transports".to_string(), json!(filters.transports));
        }
        if !filters.services.is_empty() {
            params.insert("services".to_string(), json!(filters.services));
        }
        if !ids.is_empty() {
            params.insert("ids".to_string(), json!(ids));
        }
        if let Some(payload) = payload {
            params.insert("payload".to_string(), json!(payload));
        }
        Ok(Value::Object(params))
    }

    fn request_message_page(&mut self, kind: MessagePageKind) {
        if kind == MessagePageKind::Refresh {
            self.messages.list_generation = self.messages.list_generation.wrapping_add(1);
            self.messages.loading = true;
            if !self.messages.batch_in_flight.is_empty() {
                let ids = std::mem::take(&mut self.messages.batch_in_flight);
                let state_ids = std::mem::take(&mut self.messages.batch_in_flight_state_ids);
                self.requeue_batch_parts(ids, state_ids);
            }
        }
        let generation = self.messages.list_generation;
        let (from, until, order) = match kind {
            MessagePageKind::Refresh => {
                self.messages.scroll_to_bottom = self.messages.selected.is_none();
                (None, None, MessageOrder::NewestFirst)
            }
            MessagePageKind::Older => {
                let Some(bounds) = self.messages.older_next.clone() else {
                    return;
                };
                if self.messages.loading_older {
                    return;
                }
                self.messages.loading_older = true;
                (bounds.from, bounds.until, MessageOrder::NewestFirst)
            }
            MessagePageKind::Newer => {
                let Some(bounds) = self.messages.newer_next.clone() else {
                    return;
                };
                if self.messages.loading_newer {
                    return;
                }
                self.messages.loading_newer = true;
                (bounds.from, bounds.until, MessageOrder::OldestFirst)
            }
        };
        let payload = match self.messages.filters.payload_expr() {
            Ok(payload) => payload,
            Err(error) => {
                self.messages.loading = false;
                self.messages.loading_older = false;
                self.messages.loading_newer = false;
                self.messages.error = Some(error);
                return;
            }
        };
        let params = match self.message_list_params(
            &self.messages.filters,
            payload.as_ref(),
            from,
            until,
            order,
            MESSAGE_PAGE_LIMIT,
            &[],
        ) {
            Ok(params) => params,
            Err(error) => {
                self.messages.loading = false;
                self.messages.loading_older = false;
                self.messages.loading_newer = false;
                self.messages.error = Some(error);
                return;
            }
        };
        if !self.rpc(
            RequestTag::MessagesPage { generation, kind },
            Method::MessagesList,
            params,
        ) {
            self.messages.loading = false;
            self.messages.loading_older = false;
            self.messages.loading_newer = false;
            self.messages.error = Some("not connected to the gateway".to_string());
        }
    }

    fn apply_message_page(&mut self, kind: MessagePageKind, result: MessagesListResult) {
        match kind {
            MessagePageKind::Refresh => {
                let selected = self.messages.selected;
                self.messages
                    .replace_page(result.messages, result.next, result.store_total);
                if selected.is_some_and(|id| self.messages.message_by_id(id).is_some()) {
                    self.messages.selected = selected;
                    self.messages.selected_message =
                        selected.and_then(|id| self.messages.message_by_id(id).cloned());
                    self.messages.scroll_to_selected = true;
                } else {
                    self.messages.clear_selection();
                    self.messages.scroll_to_bottom = true;
                }
                self.messages.at_bottom = true;
            }
            MessagePageKind::Older => {
                self.messages
                    .prepend_older(result.messages, result.next, result.store_total);
                if self.messages.messages.is_empty() {
                    self.messages.older_next = None;
                }
            }
            MessagePageKind::Newer => {
                let selected = self.messages.selected;
                let was_at_bottom = self.messages.at_bottom;
                self.messages
                    .append_newer(result.messages, result.next, result.store_total);
                if self.messages.messages.is_empty() {
                    self.messages.newer_next = None;
                } else if selected.is_some_and(|id| self.messages.message_by_id(id).is_some()) {
                    self.messages.selected_message =
                        selected.and_then(|id| self.messages.message_by_id(id).cloned());
                    self.messages.scroll_to_selected = true;
                } else if was_at_bottom {
                    self.messages.scroll_to_bottom = true;
                }
            }
        }
        self.messages.error = None;
    }

    fn request_reanchor(&mut self, hit: Message) {
        self.messages.list_generation = self.messages.list_generation.wrapping_add(1);
        self.messages.loading = true;
        let payload = match self.messages.filters.payload_expr() {
            Ok(payload) => payload,
            Err(error) => {
                self.messages.loading = false;
                self.messages.searching = false;
                self.messages.search_status = Some(error);
                return;
            }
        };
        let until = hit
            .time
            .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
            .unwrap_or(hit.time);
        let params = match self.message_list_params(
            &self.messages.filters,
            payload.as_ref(),
            None,
            Some(until),
            MessageOrder::NewestFirst,
            MESSAGE_PAGE_LIMIT,
            &[],
        ) {
            Ok(params) => params,
            Err(error) => {
                self.messages.loading = false;
                self.messages.searching = false;
                self.messages.search_status = Some(error);
                return;
            }
        };
        let generation = self.messages.list_generation;
        if !self.rpc(
            RequestTag::MessageAnchor {
                list_generation: generation,
                search_generation: self.messages.search_generation,
                hit: hit.id,
            },
            Method::MessagesList,
            params,
        ) {
            self.messages.loading = false;
            self.messages.searching = false;
            self.messages.search_status = Some("not connected to the gateway".to_string());
        }
    }

    fn apply_message_anchor(&mut self, hit: Uuid, result: MessagesListResult) {
        let Some(message) = result
            .messages
            .iter()
            .find(|message| message.id == hit)
            .cloned()
        else {
            self.messages.loading = false;
            self.messages.searching = false;
            self.messages.search_status =
                Some("The matching message disappeared before the page loaded.".to_string());
            self.messages.clear_selection();
            return;
        };
        self.messages
            .replace_page(result.messages, result.next, result.store_total);
        // The hit is the newest row in this anchor page. Prepare one newer page
        // so the local window does not look filtered; append_newer keeps the
        // selected hit visible instead of snapping to the bottom.
        let newer_from = self.messages.messages.last().and_then(|message| {
            message
                .time
                .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
        });
        self.messages.newer_next = newer_from.map(|from| MessageTimeRange {
            from: Some(from),
            until: None,
        });
        self.messages.selected = Some(message.id);
        self.messages.selected_message = Some(message);
        self.messages.scroll_to_selected = true;
        self.messages.searching = false;
        self.messages.search_status = None;
        self.messages.at_bottom = false;
    }

    pub(crate) fn apply_filters(&mut self) {
        self.messages.draft.error = None;
        let filters = match self.messages.draft.to_filters() {
            Ok(filters) => filters,
            Err(error) => {
                self.messages.draft.error = Some(error);
                return;
            }
        };
        self.messages.filters = filters;
        self.reload_message_list();
    }

    pub(crate) fn clear_filters(&mut self) {
        self.messages.filters = MessageFilters::default();
        self.messages.draft = FilterDraft::default();
        self.reload_message_list();
    }

    /// Applies a metadata-chip edit without touching the payload conditions.
    pub(crate) fn filters_changed(&mut self) {
        self.messages.draft = FilterDraft::from_filters(&self.messages.filters);
        self.reload_message_list();
    }

    pub(crate) fn apply_search_options(&mut self) {
        self.messages.options_draft.error = None;
        let options = match self.messages.options_draft.to_filters() {
            Ok(options) => options,
            Err(error) => {
                self.messages.options_draft.error = Some(error);
                return;
            }
        };
        self.messages.options = options;
        self.messages.options_draft = FilterDraft::from_filters(&self.messages.options);
        self.messages.invalidate_search();
    }

    pub(crate) fn clear_search_options(&mut self) {
        self.messages.options = MessageFilters::default();
        self.messages.options_draft = FilterDraft::default();
        self.messages.invalidate_search();
    }

    pub(crate) fn search_for_payload(&mut self, payload: Vec<u8>) {
        self.messages.search = hodeco_codec::hex::spaced_hex(&payload);
        self.messages.search_mode = model::PayloadMode::Binary;
        self.messages.search_error = None;
        self.messages.invalidate_search();
        self.tab = Tab::Messages;
        self.find_message(SearchDirection::Next);
    }

    pub(crate) fn find_message(&mut self, direction: SearchDirection) {
        self.messages.search_error = None;
        let query = match self.text_search_expression() {
            Ok(query) => query,
            Err(error) => {
                self.messages.search_error = Some(error);
                self.messages.searching = false;
                return;
            }
        };
        let options = match self.messages.options.payload_expr() {
            Ok(options) => options,
            Err(error) => {
                self.messages.search_error = Some(error);
                self.messages.searching = false;
                return;
            }
        };
        if query.is_none() && self.messages.options.is_empty() {
            self.messages.search_status =
                Some("Enter search text or set search options.".to_string());
            self.messages.searching = false;
            return;
        }
        let filters = match combined_search_filters(&self.messages.filters, &self.messages.options)
        {
            Ok(filters) => filters,
            Err(conflict) => {
                self.messages.searching = false;
                self.messages.search_status = Some(conflict);
                return;
            }
        };
        let filter_payload = match filters.payload_expr() {
            Ok(payload) => payload,
            Err(error) => {
                self.messages.search_error = Some(error);
                self.messages.searching = false;
                return;
            }
        };
        let combined_payload = model::and_payloads(
            [filter_payload, options, query]
                .into_iter()
                .flatten()
                .collect(),
        );
        if let Some(payload) = &combined_payload
            && let Err(error) = payload.validate()
        {
            self.messages.search_error = Some(format!("search predicate: {error}"));
            self.messages.searching = false;
            return;
        }
        let (from, until, order) = match (self.messages.selected_message(), direction) {
            (Some(message), SearchDirection::Next) => (
                message
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1)),
                None,
                MessageOrder::OldestFirst,
            ),
            (Some(message), SearchDirection::Previous) => {
                (None, Some(message.time), MessageOrder::NewestFirst)
            }
            (None, SearchDirection::Next) => (None, None, MessageOrder::OldestFirst),
            (None, SearchDirection::Previous) => (None, None, MessageOrder::NewestFirst),
        };
        self.messages.search_from_selection = self.messages.selected_message().is_some();
        self.messages.search_direction = direction;
        self.messages.search_generation = self.messages.search_generation.wrapping_add(1);
        let generation = self.messages.search_generation;
        self.messages.searching = true;
        self.messages.search_status = None;
        let params = match self.message_list_params(
            &filters,
            combined_payload.as_ref(),
            from,
            until,
            order,
            1,
            &[],
        ) {
            Ok(params) => params,
            Err(error) => {
                self.messages.searching = false;
                self.messages.search_error = Some(error);
                return;
            }
        };
        if !self.rpc(
            RequestTag::MessageSearch { generation },
            Method::MessagesList,
            params,
        ) {
            self.messages.searching = false;
            self.messages.search_error = Some("not connected to the gateway".to_string());
        }
    }

    fn text_search_expression(&self) -> Result<Option<PayloadExpr>, String> {
        let query = self.messages.search.trim();
        if query.is_empty() {
            return Ok(None);
        }
        let bytes = self
            .messages
            .search_mode
            .encode(query)
            .map_err(|error| error.to_string())?;
        if bytes.is_empty() {
            return Ok(None);
        }
        Ok(Some(PayloadExpr::anywhere(bytes)))
    }

    fn apply_search_result(&mut self, result: MessagesListResult) {
        self.messages.store_total = result.store_total;
        let Some(hit) = result.messages.into_iter().next() else {
            self.messages.searching = false;
            self.messages.search_status = Some(no_match_message(
                self.messages.search_from_selection,
                self.messages.search_direction,
            ));
            return;
        };
        self.messages.selected = Some(hit.id);
        self.messages.selected_message = Some(hit.clone());
        self.messages.scroll_to_selected = true;
        if self.messages.message_by_id(hit.id).is_some() {
            self.messages.searching = false;
            self.messages.search_status = None;
            self.messages.upsert_messages(&[hit]);
        } else {
            self.request_reanchor(hit);
        }
    }

    fn queue_message_notification(&mut self, id: Uuid, kind: PendingNotification, now: Instant) {
        self.messages
            .pending_notifications
            .entry(id)
            .and_modify(|existing| {
                if kind == PendingNotification::StateChanged {
                    *existing = PendingNotification::StateChanged;
                }
            })
            .or_insert(kind);
        let due = now + Duration::from_millis(100);
        self.messages.notification_due_at = Some(
            self.messages
                .notification_due_at
                .map_or(due, |current| current.min(due)),
        );
    }

    fn flush_notification_batch(&mut self, now: Instant) {
        if self.messages.loading || !self.messages.batch_in_flight.is_empty() {
            return;
        }
        let Some(due) = self.messages.notification_due_at else {
            return;
        };
        if now < due {
            return;
        }
        let mut ids = Vec::new();
        let mut state_ids = HashSet::new();
        let pending = std::mem::take(&mut self.messages.pending_notifications);
        for (id, kind) in pending {
            if ids.len() >= MESSAGE_NOTIFICATION_BATCH as usize {
                self.messages.pending_notifications.insert(id, kind);
                continue;
            }
            if kind == PendingNotification::StateChanged {
                state_ids.insert(id);
            }
            ids.push(id);
        }
        if ids.is_empty() {
            self.messages.notification_due_at = None;
            return;
        }
        self.messages.batch_in_flight = ids.clone();
        self.messages.batch_in_flight_state_ids = state_ids;
        self.messages.notification_due_at = None;
        let payload = match self.messages.filters.payload_expr() {
            Ok(payload) => payload,
            Err(error) => {
                self.messages.error = Some(error);
                self.requeue_batch(ids);
                return;
            }
        };
        let params = match self.message_list_params(
            &self.messages.filters,
            payload.as_ref(),
            None,
            None,
            MessageOrder::OldestFirst,
            MESSAGE_NOTIFICATION_BATCH,
            &ids,
        ) {
            Ok(params) => params,
            Err(error) => {
                self.messages.error = Some(error);
                self.requeue_batch(ids);
                return;
            }
        };
        let generation = self.messages.list_generation;
        if !self.rpc(
            RequestTag::MessageBatch { generation },
            Method::MessagesList,
            params,
        ) {
            self.requeue_batch(ids);
        }
    }

    fn requeue_batch(&mut self, ids: Vec<Uuid>) {
        let state_ids = self.messages.batch_in_flight_state_ids.clone();
        self.requeue_batch_parts(ids, state_ids);
    }

    fn requeue_batch_parts(&mut self, ids: Vec<Uuid>, state_ids: HashSet<Uuid>) {
        for id in ids {
            let kind = if state_ids.contains(&id) {
                PendingNotification::StateChanged
            } else {
                PendingNotification::New
            };
            self.messages
                .pending_notifications
                .entry(id)
                .and_modify(|existing| {
                    if kind == PendingNotification::StateChanged {
                        *existing = kind;
                    }
                })
                .or_insert(kind);
        }
        self.messages.batch_in_flight.clear();
        self.messages.batch_in_flight_state_ids.clear();
        self.messages.notification_due_at = Some(Instant::now() + Duration::from_millis(200));
    }

    fn apply_notification_batch(&mut self, result: MessagesListResult) {
        let returned: HashSet<Uuid> = result.messages.iter().map(|message| message.id).collect();
        let was_at_bottom = self.messages.at_bottom;
        self.messages.upsert_messages(&result.messages);
        if was_at_bottom {
            self.messages.scroll_to_bottom = true;
        }
        let missing: Vec<Uuid> = self
            .messages
            .batch_in_flight_state_ids
            .iter()
            .copied()
            .filter(|id| !returned.contains(id))
            .collect();
        if !missing.is_empty() {
            self.messages.remove_ids(&missing);
        }
        self.messages.store_total = result.store_total;
        self.messages.batch_in_flight.clear();
        self.messages.batch_in_flight_state_ids.clear();
        if !self.messages.pending_notifications.is_empty() {
            self.messages.notification_due_at = Some(Instant::now());
        }
    }

    pub(crate) fn poll_message_requests(&mut self, now: Instant) {
        self.flush_notification_batch(now);
    }
    pub(crate) fn send_service_payload(
        &mut self,
        service: &ServiceRef,
        payload: Vec<u8>,
        ttl: Option<HumanDuration>,
    ) {
        let mut params = json!({
            "service": service,
            "payload": Base64Bytes::from(payload),
        });
        if let Some(ttl) = ttl {
            params["ttl"] = json!(ttl);
        }
        self.rpc(RequestTag::ServiceSend, Method::ServicesSend, params);
    }

    pub(crate) fn send_console_request(&mut self) {
        let method_name = self.console.method.trim().to_string();
        let method = match method_name.parse::<Method>() {
            Ok(method) => method,
            Err(error) => {
                self.console.error = Some(error.message.clone());
                return;
            }
        };
        let params: Value = match parse_console_params(&self.console.params) {
            Ok(params) => params,
            Err(error) => {
                self.console.error = Some(error);
                return;
            }
        };
        let id = self.console.next_id;
        self.console.next_id += 1;
        self.console.entries.push(ConsoleEntry {
            id,
            time: Utc::now(),
            method: method_name,
            params: self.console.params.clone(),
            outcome: ConsoleOutcome::Pending,
        });
        self.console.pending = true;
        self.console.expand_id = Some(id);
        self.console.error = None;
        if !self.rpc(RequestTag::RawConsole, method, params) {
            self.console.pending = false;
            self.finish_console(ConsoleOutcome::LocalError("not connected".to_string()));
        }
    }

    fn finish_console(&mut self, outcome: ConsoleOutcome) {
        if let Some(entry) = self
            .console
            .entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry.outcome, ConsoleOutcome::Pending))
        {
            entry.outcome = outcome;
        }
    }

    pub(crate) fn start_discovery(&mut self) {
        if self.discovery.protocol.is_empty() || self.discovery.transport.is_empty() {
            self.discovery.error =
                Some("select a protocol and transport before starting discovery".to_string());
            return;
        }
        let params = match self.discovery.editor.as_mut() {
            Some(editor) => match editor.collect() {
                Ok(params) => params,
                Err(errors) => {
                    self.discovery.error = Some(errors.join("; "));
                    return;
                }
            },
            None => ParamValues::new(),
        };
        self.discovery.starting = true;
        self.discovery.error = None;
        self.discovery.viewing = None;
        let params = json!({
            "protocol": self.discovery.protocol,
            "transport": self.discovery.transport,
            "params": params,
        });
        if !self.rpc(RequestTag::DiscoveryStart, Method::DiscoveryStart, params) {
            self.discovery.starting = false;
        }
    }

    pub(crate) fn cancel_discovery(&mut self) {
        let Some(discovery) = &self.discovery.current else {
            return;
        };
        if discovery.state != DiscoveryState::Running {
            return;
        }
        let id = discovery.id;
        self.rpc(
            RequestTag::DiscoveryCancel,
            Method::DiscoveryCancel,
            json!({"discoveryId": id}),
        );
    }

    pub(crate) fn add_service(
        &mut self,
        dialog: &AddServiceDialog,
        service_params: ParamValues,
    ) -> bool {
        let alias = dialog.alias.trim();
        let mut params = json!({
            "protocol": dialog.protocol,
            "transport": dialog.transport,
            "entry": dialog.entry.trim(),
        });
        if !alias.is_empty() {
            params["alias"] = json!(alias);
        }
        if !service_params.is_empty() {
            params["params"] = json!(service_params);
        }
        self.rpc(RequestTag::ServiceAdd, Method::ServicesAdd, params)
    }

    pub(crate) fn save_alias(&mut self, dialog: &EditAliasDialog) -> bool {
        let alias = dialog.alias.trim();
        let mut params = json!({"service": dialog.service});
        if !alias.is_empty() {
            params["alias"] = json!(alias);
        }
        self.rpc(RequestTag::ServiceAlias, Method::ServicesAlias, params)
    }

    pub(crate) fn restart_server(&mut self) {
        if self.rpc(RequestTag::RawConsole, Method::SystemRestart, json!({})) {
            self.restarting = true;
            self.toast(
                Tone::Info,
                "Restart requested; waiting for the gateway to return.",
            );
        }
    }

    pub(crate) fn connect(&mut self, endpoint: &str, demo: bool) -> Result<(), String> {
        let endpoint = if demo {
            endpoint.trim().to_string()
        } else {
            endpoint::normalize(endpoint).map_err(|error| error.to_string())?
        };
        self.reset_data();
        self.gateway.reconnect(endpoint, demo, Instant::now());
        self.dialog = None;
        self.toast(
            Tone::Info,
            if demo {
                "Starting demo mode with fixture data."
            } else {
                "Connecting to the gateway."
            },
        );
        Ok(())
    }

    fn reset_data(&mut self) {
        self.server = None;
        self.protocols.clear();
        self.transports.clear();
        self.services.clear();
        self.config = None;
        self.messages = MessagesUi::default();
        self.discovery = DiscoveryUi::default();
        self.console.entries.clear();
        self.console.pending = false;
        self.console.expand_id = None;
        self.last_error = None;
        self.last_sync = None;
        self.protocol_mismatch = false;
        self.stale = false;
        self.restarting = false;
        self.discovery_poll_at = None;
    }

    fn upsert_service(&mut self, service: ServiceInfo) {
        if let Some(existing) = self.services.iter_mut().find(|existing| {
            existing.protocol == service.protocol
                && existing.transport == service.transport
                && existing.entry == service.entry
        }) {
            *existing = service;
        } else {
            self.services.push(service);
        }
    }

    fn upsert_discovery(&mut self, discovery: DiscoveryInfo) {
        if let Some(existing) = self
            .discovery
            .history
            .iter_mut()
            .find(|existing| existing.info.id == discovery.id)
        {
            existing.info = discovery.clone();
            existing.seen_at = Utc::now();
        } else {
            self.discovery.history.push(DiscoveryHistoryEntry {
                info: discovery.clone(),
                seen_at: Utc::now(),
            });
            if self.discovery.history.len() > 20 {
                self.discovery.history.remove(0);
            }
        }
        self.discovery.current = Some(discovery.clone());
        let running = discovery.state == DiscoveryState::Running;
        self.discovery.starting = running;
        self.discovery_poll_at = running.then(|| Instant::now() + Duration::from_secs(1));
        if running {
            self.discovery.error = None;
        }
    }

    fn ensure_selection_defaults(&mut self) {
        if !self
            .protocols
            .iter()
            .any(|protocol| protocol.id == self.discovery.protocol)
        {
            self.discovery.protocol = self
                .protocols
                .first()
                .map(|protocol| protocol.id.clone())
                .unwrap_or_default();
        }
        let supported = model::supported_transports(
            &self.protocols,
            &self.discovery.protocol,
            &self.transports,
        );
        if !supported
            .iter()
            .any(|transport| transport.id == self.discovery.transport)
        {
            self.discovery.transport = supported
                .first()
                .map(|transport| transport.id.clone())
                .unwrap_or_default();
        }
    }

    // -- event reduction ---------------------------------------------------

    fn reduce(&mut self, event: GatewayEvent, now: Instant) {
        match event {
            GatewayEvent::Connected => {
                self.messages.reset();
            }
            GatewayEvent::Synced => {
                self.last_sync = Some(Utc::now());
                self.stale = false;
                if self.restarting {
                    self.restarting = false;
                    self.config_ui.restart_required.clear();
                    self.config_ui.saved = false;
                    self.toast(Tone::Success, "The gateway is back online.");
                }
                self.request_message_page(MessagePageKind::Refresh);
                self.refresh_runs();
                if self
                    .discovery
                    .current
                    .as_ref()
                    .is_some_and(|discovery| discovery.state == DiscoveryState::Running)
                {
                    self.discovery_poll_at = Some(now);
                    self.rpc(RequestTag::DiscoveryGet, Method::DiscoveryGet, json!({}));
                }
                self.ensure_selection_defaults();
            }
            GatewayEvent::Disconnected {
                reason,
                unknown_mutations,
            } => {
                self.last_error = Some(reason.clone());
                self.stale = self.last_sync.is_some();
                self.messages.reset();
                self.messages.loading = false;
                self.tasks.tasks.clear();
                self.tasks.loading = false;
                self.tasks.error = None;
                self.tasks.selected = None;
                self.tasks.history = None;
                self.schedules.schedules.clear();
                self.schedules.loading = false;
                self.schedules.error = None;
                self.schedules.selected = None;
                self.commands.commands.clear();
                self.commands.loading = false;
                self.commands.error = None;
                self.commands.selected = None;
                self.runs.runs.clear();
                self.runs.running.clear();
                self.runs.loading = false;
                self.runs.loading_older = false;
                self.runs.loading_newer = false;
                self.runs.older_next = None;
                self.runs.newer_next = None;
                self.runs.feed_rows = 0;
                self.runs.feed_bytes = 0;
                self.runs.at_bottom = true;
                self.runs.scroll_to_bottom = false;
                self.runs.scroll_anchor = None;
                self.runs.detail = None;
                self.runs.selected = None;
                self.runs.selected_running = None;
                self.runs.selected_index = None;
                self.runs.cancelling.clear();
                self.runs.pending_new = 0;
                self.runs.pending_feed.clear();
                self.runs.pending_dropped.clear();
                if let Some(Dialog::Message(dialog)) = &mut self.dialog
                    && dialog.loading
                {
                    dialog.loading = false;
                    dialog.error =
                        Some("connection lost before the message was loaded".to_string());
                }
                self.discovery_poll_at = None;
                self.console.pending = false;
                self.protocol_params_requested.clear();
                self.protocol_params_errors.clear();
                self.config_ui.saving = false;
                self.finish_console(ConsoleOutcome::Timeout);
                if !unknown_mutations.is_empty() {
                    let labels: Vec<String> = unknown_mutations.iter().map(tag_label).collect();
                    self.toast(
                        Tone::Warning,
                        format!(
                            "Connection lost while waiting for {}. The outcome is unknown; data will be reloaded after reconnect.",
                            labels.join(", ")
                        ),
                    );
                }
            }
            GatewayEvent::Response { tag, result } => self.handle_response(tag, result),
            GatewayEvent::ErrorResponse { tag, error } => self.handle_error(tag, error),
            GatewayEvent::Timeout { tag } => self.handle_timeout(tag),
            GatewayEvent::Notification(notification) => self.handle_notification(notification),
            GatewayEvent::BadMessage(text) => {
                log::warn!("{text}");
                self.toast(Tone::Warning, text);
            }
        }
    }

    fn handle_response(&mut self, tag: RequestTag, result: Value) {
        match tag {
            RequestTag::Subscribe => {}
            RequestTag::ServerInfo => match parse_result::<ServerInfoResult>(result) {
                Ok(server) => {
                    self.protocol_mismatch = server.protocol_version != EXPECTED_PROTOCOL_VERSION;
                    self.server = Some(server);
                }
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::ProtocolList => match parse_result::<ProtocolsListResult>(result) {
                Ok(list) => {
                    self.protocols = list.protocols;
                    self.ensure_selection_defaults();
                }
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::TransportList => match parse_result::<TransportsListResult>(result) {
                Ok(list) => {
                    self.transports = list.transports;
                    self.ensure_selection_defaults();
                }
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::Config => match parse_result::<Config>(result) {
                Ok(config) => {
                    if !self.config_ui.dirty {
                        self.config_ui.load(&config);
                    }
                    self.config = Some(config);
                }
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::ConfigUpdate => match parse_result::<ConfigUpdateResult>(result) {
                Ok(update) => {
                    self.config = Some(update.config.clone());
                    self.config_ui
                        .apply_saved(&update.config, update.restart_required);
                    if matches!(&self.dialog, Some(Dialog::ConfigDirty)) {
                        self.dialog = None;
                    }
                    if let Some(tab) = self.pending_tab.take() {
                        if self.tab == Tab::Messages && tab != Tab::Messages {
                            self.messages.list_keyboard = false;
                        }
                        self.tab = tab;
                    }
                    self.toast(Tone::Success, "Configuration saved.");
                }
                Err(error) => {
                    self.config_ui.saving = false;
                    self.config_ui.error = Some(error.clone());
                    if !matches!(&self.dialog, Some(Dialog::ConfigDirty)) {
                        self.toast(Tone::Error, error);
                    }
                }
            },
            RequestTag::ServiceList => match parse_result::<ServicesListResult>(result) {
                Ok(list) => self.services = list.services,
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::TasksList => match parse_result::<TasksListResult>(result) {
                Ok(list) => {
                    self.tasks.loading = false;
                    self.tasks.error = None;
                    self.tasks.tasks = list.tasks;
                    self.tasks
                        .tasks
                        .sort_by(|left, right| left.task.name.cmp(&right.task.name));
                    if self.gateway.is_demo() {
                        let running: Vec<String> = self
                            .tasks
                            .tasks
                            .iter()
                            .filter(|info| info.running)
                            .map(|info| info.task.id.clone())
                            .collect();
                        for task_id in running {
                            if self.runs.running.iter().any(|run| run.task_id == task_id) {
                                continue;
                            }
                            self.runs.running.push(views::runs::RunningRun {
                                id: self
                                    .tasks
                                    .task(&task_id)
                                    .and_then(|info| info.running_run_id)
                                    .unwrap_or_else(Uuid::new_v4),
                                task_id,
                                trigger: hodeco_contract::domain::TriggerRef {
                                    id: "demo".to_string(),
                                    kind: hodeco_contract::domain::TriggerType::Manual,
                                },
                                started_at: Utc::now(),
                                delayed_until: None,
                            });
                        }
                    }
                }
                Err(error) => {
                    self.tasks.loading = false;
                    self.tasks.error = Some(error);
                }
            },
            RequestTag::TaskAdd => match parse_result::<TasksAddResult>(result) {
                Ok(added) => {
                    replace_sorted(
                        &mut self.tasks.tasks,
                        added.task,
                        |info| info.task.id.as_str(),
                        |info| info.task.name.as_str(),
                    );
                    self.tasks.editor = None;
                    self.toast(Tone::Success, "Task created.");
                }
                Err(error) => {
                    if let Some(editor) = self.tasks.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::TaskUpdate => match parse_result::<TasksUpdateResult>(result) {
                Ok(updated) => {
                    replace_sorted(
                        &mut self.tasks.tasks,
                        updated.task.clone(),
                        |info| info.task.id.as_str(),
                        |info| info.task.name.as_str(),
                    );
                    if let Some(editor) = self.tasks.editor.as_mut() {
                        editor.saving = false;
                        editor.original = Some(updated.task.task.clone());
                        editor.draft = updated.task.task;
                        editor.error = None;
                    }
                    self.toast(Tone::Success, "Task saved.");
                }
                Err(error) => {
                    if let Some(editor) = self.tasks.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::TaskRemove { task_id } => {
                self.tasks.tasks.retain(|info| info.task.id != task_id);
                if self
                    .tasks
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.draft.id == task_id)
                {
                    self.tasks.editor = None;
                }
                self.tasks.confirm_delete = None;
                if self.runs.filter_task.as_deref() == Some(task_id.as_str()) {
                    self.runs.filter_task = None;
                    self.refresh_runs();
                }
                self.toast(Tone::Success, "Task deleted.");
            }
            RequestTag::TaskFire => {
                self.tasks.firing = false;
                match parse_result::<TasksFireResult>(result) {
                    Ok(_) => self.toast(Tone::Success, "Task started."),
                    Err(error) => {
                        if let Some(editor) = self.tasks.editor.as_mut()
                            && let Some(dialog) = editor.run_now.as_mut()
                        {
                            dialog.sending = false;
                            dialog.error = Some(error.clone());
                        }
                        self.toast(Tone::Error, error);
                    }
                }
            }
            RequestTag::TaskCancel { run_id, .. } => {
                self.runs.cancelling.remove(&run_id);
                self.toast(Tone::Success, "Cancellation requested.");
            }
            RequestTag::TaskPurgeHistory { .. } => {
                self.refresh_tasks();
                self.refresh_runs();
                self.toast(Tone::Success, "Task history purged.");
            }
            RequestTag::TaskRunFeed { generation, kind } => {
                if generation != self.runs.generation {
                    return;
                }
                self.runs.loading = false;
                self.runs.loading_older = false;
                self.runs.loading_newer = false;
                match parse_result::<TasksRunFeedResult>(result) {
                    Ok(page) => self.apply_run_feed_page(kind, page),
                    Err(error) => self.runs.error = Some(error),
                }
            }
            RequestTag::TaskHistoryPage { generation, kind } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if generation != dialog.generation {
                    return;
                }
                dialog.loading = false;
                dialog.loading_older = false;
                dialog.loading_newer = false;
                match parse_result::<TasksHistoryResult>(result) {
                    Ok(page) => {
                        let cap = views::task_history::WINDOW_CAP;
                        match kind {
                            RunPageKind::Refresh => {
                                dialog.runs = views::runs::sorted_runs(page.runs);
                                dialog.older_next = page.next;
                                dialog.newer_next = None;
                                dialog.scroll_anchor = None;
                                dialog.scroll_to_bottom = dialog.selected.is_none();
                                dialog.at_bottom = true;
                            }
                            RunPageKind::Older => {
                                dialog.scroll_anchor = dialog.runs.first().map(|run| run.id);
                                let mut merged = page.runs;
                                merged.extend(std::mem::take(&mut dialog.runs));
                                dialog.runs = views::runs::sorted_runs(merged);
                                dialog.older_next = page.next;
                                if dialog.runs.len() > cap {
                                    let excess = dialog.runs.len() - cap;
                                    let from = dialog.runs[dialog.runs.len() - excess].time;
                                    dialog.runs.truncate(cap);
                                    dialog.newer_next = Some(MessageTimeRange {
                                        from: Some(from),
                                        until: dialog
                                            .newer_next
                                            .as_ref()
                                            .and_then(|next| next.until),
                                    });
                                }
                            }
                            RunPageKind::Newer => {
                                let mut merged = std::mem::take(&mut dialog.runs);
                                merged.extend(page.runs);
                                dialog.runs = views::runs::sorted_runs(merged);
                                dialog.newer_next = page.next;
                                if dialog.runs.len() > cap {
                                    let excess = dialog.runs.len() - cap;
                                    let until = dialog.runs[excess - 1]
                                        .time
                                        .checked_add_signed(chrono::TimeDelta::nanoseconds(1));
                                    dialog.runs.drain(0..excess);
                                    dialog.older_next = Some(MessageTimeRange {
                                        from: dialog.older_next.as_ref().and_then(|next| next.from),
                                        until,
                                    });
                                }
                                if dialog.selected.is_none() {
                                    dialog.scroll_to_bottom = true;
                                }
                            }
                        }
                        if dialog
                            .selected
                            .is_some_and(|id| !dialog.runs.iter().any(|run| run.id == id))
                        {
                            dialog.selected = None;
                            dialog.detail = None;
                        }
                        dialog.error = None;
                    }
                    Err(error) => dialog.error = Some(error),
                }
            }
            RequestTag::TaskHistoryRunGet { task_id, run_id } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if dialog.task_id != task_id || dialog.selected != Some(run_id) {
                    return;
                }
                dialog.detail_loading = false;
                match parse_result::<TasksGetRunResult>(result) {
                    Ok(result) => {
                        dialog.detail = Some(result.run);
                        dialog.detail_error = None;
                    }
                    Err(error) => dialog.detail_error = Some(error),
                }
            }
            RequestTag::TaskRunGet { task_id, run_id } => {
                if !self
                    .runs
                    .selected
                    .as_ref()
                    .is_some_and(|(task, run)| task == &task_id && *run == run_id)
                {
                    return;
                }
                self.runs.detail_loading = false;
                match parse_result::<TasksGetRunResult>(result) {
                    Ok(fetched) => {
                        self.runs.detail = Some(fetched.run);
                        self.runs.detail_error = None;
                    }
                    Err(error) => self.runs.detail_error = Some(error),
                }
            }
            RequestTag::SchedulesList => match parse_result::<SchedulesListResult>(result) {
                Ok(list) => {
                    self.schedules.loading = false;
                    self.schedules.error = None;
                    self.schedules.schedules = list.schedules;
                    self.schedules.defaults_location = list.location;
                    self.schedules.defaults_timezone = list.timezone;
                    self.schedules.system_timezone = list.system_timezone;
                    self.schedules
                        .schedules
                        .sort_by(|left, right| left.schedule.name.cmp(&right.schedule.name));
                }
                Err(error) => {
                    self.schedules.loading = false;
                    self.schedules.error = Some(error);
                }
            },
            RequestTag::ScheduleAdd => match parse_result::<SchedulesAddResult>(result) {
                Ok(added) => {
                    replace_sorted(
                        &mut self.schedules.schedules,
                        added.schedule,
                        |info| info.schedule.id.as_str(),
                        |info| info.schedule.name.as_str(),
                    );
                    self.schedules.editor = None;
                    self.toast(Tone::Success, "Schedule created.");
                }
                Err(error) => {
                    if let Some(editor) = self.schedules.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::ScheduleUpdate => match parse_result::<SchedulesUpdateResult>(result) {
                Ok(updated) => {
                    replace_sorted(
                        &mut self.schedules.schedules,
                        updated.schedule.clone(),
                        |info| info.schedule.id.as_str(),
                        |info| info.schedule.name.as_str(),
                    );
                    if let Some(editor) = self.schedules.editor.as_mut() {
                        editor.saving = false;
                        editor.original = Some(updated.schedule.schedule.clone());
                        editor.draft = updated.schedule.schedule;
                        editor.error = None;
                    }
                    self.refresh_tasks();
                    self.toast(Tone::Success, "Schedule saved.");
                }
                Err(error) => {
                    if let Some(editor) = self.schedules.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::ScheduleRemove { schedule_id } => {
                self.schedules
                    .schedules
                    .retain(|info| info.schedule.id != schedule_id);
                if self
                    .schedules
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.draft.id == schedule_id)
                {
                    self.schedules.editor = None;
                }
                self.schedules.confirm_delete = None;
                self.refresh_tasks();
                self.toast(Tone::Success, "Schedule deleted.");
            }
            RequestTag::SchedulePreview { generation } => {
                if let Some(editor) = self.schedules.editor.as_mut()
                    && editor.preview_generation == generation
                {
                    editor.previewing = false;
                    match parse_result::<SchedulesPreviewResult>(result) {
                        Ok(preview) => {
                            editor.preview = preview.occurrences;
                            editor.preview_error = None;
                        }
                        Err(error) => {
                            editor.preview.clear();
                            editor.preview_error = Some(error);
                        }
                    }
                }
            }
            RequestTag::ScheduleSetDefaults => {
                match parse_result::<SchedulesSetDefaultsResult>(result) {
                    Ok(defaults) => {
                        self.schedules.defaults_editor = None;
                        self.schedules.defaults_location = defaults.location;
                        self.schedules.defaults_timezone = defaults.timezone;
                        self.toast(Tone::Success, "Schedule defaults saved.");
                        self.refresh_schedules();
                        self.refresh_tasks();
                    }
                    Err(error) => {
                        if let Some(editor) = self.schedules.defaults_editor.as_mut() {
                            editor.saving = false;
                            editor.error = Some(error.clone());
                        }
                        self.toast(Tone::Error, error);
                    }
                }
            }
            RequestTag::SolarPreview { generation } => {
                if let Some(dialog) = self.schedules.location_dialog.as_mut()
                    && dialog.preview_generation == generation
                {
                    dialog.previewing = false;
                    match parse_result::<SchedulesSolarPreviewResult>(result) {
                        Ok(preview) => {
                            dialog.preview = Some(preview);
                            dialog.preview_error = None;
                        }
                        Err(error) => dialog.preview_error = Some(error),
                    }
                }
            }
            RequestTag::CommandsList => match parse_result::<CommandsListResult>(result) {
                Ok(list) => {
                    self.commands.loading = false;
                    self.commands.error = None;
                    self.commands.commands = list.commands;
                    self.commands
                        .commands
                        .sort_by(|left, right| left.command.name.cmp(&right.command.name));
                }
                Err(error) => {
                    self.commands.loading = false;
                    self.commands.error = Some(error);
                }
            },
            RequestTag::CommandAdd => match parse_result::<CommandsAddResult>(result) {
                Ok(added) => {
                    replace_sorted(
                        &mut self.commands.commands,
                        added.command,
                        |info| info.command.id.as_str(),
                        |info| info.command.name.as_str(),
                    );
                    self.commands.editor = None;
                    self.toast(Tone::Success, "Command created.");
                }
                Err(error) => {
                    if let Some(editor) = self.commands.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::CommandUpdate => match parse_result::<CommandsUpdateResult>(result) {
                Ok(updated) => {
                    replace_sorted(
                        &mut self.commands.commands,
                        updated.command.clone(),
                        |info| info.command.id.as_str(),
                        |info| info.command.name.as_str(),
                    );
                    if let Some(editor) = self.commands.editor.as_mut() {
                        editor.saving = false;
                        editor.draft = updated.command.command;
                        editor.error = None;
                    }
                    self.refresh_tasks();
                    self.toast(Tone::Success, "Command saved.");
                }
                Err(error) => {
                    if let Some(editor) = self.commands.editor.as_mut() {
                        editor.saving = false;
                        editor.error = Some(error.clone());
                    }
                    self.toast(Tone::Error, error);
                }
            },
            RequestTag::CommandRemove { command_id } => {
                self.commands
                    .commands
                    .retain(|info| info.command.id != command_id);
                if self
                    .commands
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.draft.id == command_id)
                {
                    self.commands.editor = None;
                }
                self.commands.confirm_delete = None;
                self.refresh_tasks();
                self.toast(Tone::Success, "Command deleted.");
            }
            RequestTag::MessagesPage { generation, kind } => {
                if generation != self.messages.list_generation {
                    return;
                }
                match parse_result::<MessagesListResult>(result) {
                    Ok(page) => self.apply_message_page(kind, page),
                    Err(error) => {
                        self.messages.loading = false;
                        self.messages.loading_older = false;
                        self.messages.loading_newer = false;
                        self.messages.error = Some(error);
                    }
                }
            }
            RequestTag::MessageSearch { generation } => {
                if generation != self.messages.search_generation {
                    return;
                }
                match parse_result::<MessagesListResult>(result) {
                    Ok(page) => self.apply_search_result(page),
                    Err(error) => {
                        self.messages.searching = false;
                        self.messages.search_error = Some(error);
                    }
                }
            }
            RequestTag::MessageBatch { generation } => {
                if generation != self.messages.list_generation {
                    return;
                }
                match parse_result::<MessagesListResult>(result) {
                    Ok(page) => self.apply_notification_batch(page),
                    Err(error) => {
                        let ids = std::mem::take(&mut self.messages.batch_in_flight);
                        self.messages.batch_in_flight_state_ids.clear();
                        self.messages.error = Some(error);
                        self.requeue_batch(ids);
                    }
                }
            }
            RequestTag::MessageAnchor {
                list_generation,
                search_generation,
                hit,
            } => {
                if list_generation != self.messages.list_generation
                    || search_generation != self.messages.search_generation
                {
                    return;
                }
                match parse_result::<MessagesListResult>(result) {
                    Ok(page) => self.apply_message_anchor(hit, page),
                    Err(error) => {
                        self.messages.loading = false;
                        self.messages.searching = false;
                        self.messages.search_error = Some(error);
                    }
                }
            }
            RequestTag::MessageGet { id } => {
                let result =
                    parse_result::<MessagesGetResult>(result).map(|fetched| fetched.message);
                self.finish_message_fetch(id, result);
            }
            RequestTag::ServiceAdd => match parse_result::<ServicesAddResult>(result) {
                Ok(added) => {
                    self.upsert_service(added.service);
                    self.dialog = None;
                    self.refresh_config();
                    self.toast(Tone::Success, "Service added.");
                }
                Err(error) => self.dialog_error(error),
            },
            RequestTag::ServiceRemove => {
                self.refresh_services();
                self.refresh_config();
                self.toast(Tone::Success, "Service removed.");
            }
            RequestTag::ServiceAlias => match parse_result::<ServicesAliasResult>(result) {
                Ok(updated) => {
                    self.upsert_service(updated.service);
                    self.dialog = None;
                    self.refresh_config();
                    self.toast(Tone::Success, "Alias updated.");
                }
                Err(error) => self.dialog_error(error),
            },
            RequestTag::ServiceStatus { service } => {
                match parse_result::<ServicesStatusResult>(result) {
                    Ok(status) => {
                        if let Some(service) = model::find_service_mut(&mut self.services, &service)
                        {
                            service.status = status.status;
                        }
                    }
                    Err(error) => self.toast(Tone::Error, error),
                }
            }
            RequestTag::ServiceParams { .. } => {
                match parse_result::<ServicesParamsResult>(result) {
                    Ok(updated) => {
                        self.upsert_service(updated.service);
                        if matches!(&self.dialog, Some(Dialog::ServiceParams(_))) {
                            self.dialog = None;
                        }
                        self.refresh_config();
                        self.toast(Tone::Success, "Service parameters updated.");
                    }
                    Err(error) => {
                        if let Some(Dialog::ServiceParams(dialog)) = &mut self.dialog {
                            dialog.pending = false;
                            dialog.error = Some(error.clone());
                        } else {
                            self.toast(Tone::Error, error);
                        }
                    }
                }
            }
            RequestTag::ServiceSend => match parse_result::<ServicesSendResult>(result) {
                Ok(sent) => {
                    self.toast(
                        Tone::Success,
                        format!(
                            "Message {} queued for sending.",
                            model::short_id(sent.message.id)
                        ),
                    );
                }
                Err(error) => self.toast(Tone::Error, error),
            },
            RequestTag::DiscoveryStart => match parse_result::<DiscoveryStartResult>(result) {
                Ok(started) => {
                    self.discovery.starting = true;
                    self.discovery.error = None;
                    self.discovery_poll_at = Some(Instant::now());
                    log::debug!("started discovery {}", started.discovery_id);
                }
                Err(error) => {
                    self.discovery.starting = false;
                    self.discovery.error = Some(error);
                }
            },
            RequestTag::DiscoveryGet => match parse_result::<DiscoveryGetResult>(result) {
                Ok(found) => self.upsert_discovery(found.discovery),
                Err(error) => {
                    self.discovery.error = Some(error);
                    self.discovery_poll_at = None;
                }
            },
            RequestTag::DiscoveryCancel => {
                self.discovery.starting = false;
                self.toast(Tone::Info, "Discovery cancelled.");
            }
            RequestTag::ProtocolParams {
                protocol,
                transport,
            } => {
                let key = (protocol, transport);
                self.protocol_params_requested.remove(&key);
                match parse_result::<ProtocolParamsResult>(result) {
                    Ok(params) => {
                        self.protocol_params_errors.remove(&key);
                        self.protocol_params.insert(key, params.params);
                    }
                    Err(error) => {
                        self.protocol_params_errors.insert(key, error);
                    }
                }
            }
            RequestTag::RawConsole => {
                self.console.pending = false;
                self.finish_console(ConsoleOutcome::Success(result));
            }
        }
    }

    fn handle_error(&mut self, tag: RequestTag, error: ApiError) {
        match &tag {
            RequestTag::MessagesPage { generation, .. }
                if *generation == self.messages.list_generation =>
            {
                self.messages.loading = false;
                self.messages.loading_older = false;
                self.messages.loading_newer = false;
                self.messages.error = Some(error.message.clone());
            }
            RequestTag::MessageSearch { generation }
                if *generation == self.messages.search_generation =>
            {
                self.messages.searching = false;
                self.messages.search_error = Some(error.message.clone());
            }
            RequestTag::MessageBatch { generation }
                if *generation == self.messages.list_generation =>
            {
                let ids = std::mem::take(&mut self.messages.batch_in_flight);
                let state_ids = std::mem::take(&mut self.messages.batch_in_flight_state_ids);
                self.messages.error = Some(error.message.clone());
                self.requeue_batch_parts(ids, state_ids);
            }
            RequestTag::MessageAnchor {
                list_generation,
                search_generation,
                ..
            } if *list_generation == self.messages.list_generation
                && *search_generation == self.messages.search_generation =>
            {
                self.messages.loading = false;
                self.messages.searching = false;
                self.messages.search_error = Some(error.message.clone());
            }
            RequestTag::MessageGet { id } => {
                self.finish_message_fetch(*id, Err(error.message.clone()));
            }
            RequestTag::MessagesPage { .. }
            | RequestTag::MessageSearch { .. }
            | RequestTag::MessageBatch { .. }
            | RequestTag::MessageAnchor { .. } => (),
            RequestTag::ConfigUpdate => {
                self.config_ui.saving = false;
                self.config_ui.error = Some(error.message.clone());
                if !matches!(&self.dialog, Some(Dialog::ConfigDirty)) {
                    self.toast(Tone::Error, error.message);
                }
            }
            RequestTag::ServiceParams { .. } => {
                if let Some(Dialog::ServiceParams(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(error.message.clone());
                } else {
                    self.toast(Tone::Error, error.message);
                }
            }
            RequestTag::ProtocolParams {
                protocol,
                transport,
            } => {
                let key = (protocol.clone(), transport.clone());
                self.protocol_params_requested.remove(&key);
                self.protocol_params_errors
                    .insert(key, error.message.clone());
            }
            RequestTag::ServiceAdd => {
                if let Some(Dialog::AddService(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(error.message.clone());
                } else {
                    self.toast(Tone::Error, error.message);
                }
            }
            RequestTag::ServiceAlias => {
                if let Some(Dialog::EditAlias(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(error.message.clone());
                } else {
                    self.toast(Tone::Error, error.message);
                }
            }
            RequestTag::DiscoveryStart | RequestTag::DiscoveryGet => {
                self.discovery.starting = false;
                self.discovery.error = Some(error.message.clone());
                self.discovery_poll_at = None;
            }
            RequestTag::RawConsole => {
                self.console.pending = false;
                self.console.error = Some(error.message.clone());
                self.finish_console(ConsoleOutcome::Error(error));
            }
            RequestTag::ServiceSend => self.toast(Tone::Error, error.message),
            RequestTag::TasksList => {
                self.tasks.loading = false;
                self.tasks.error = Some(error.message.clone());
            }
            RequestTag::TaskAdd | RequestTag::TaskUpdate => {
                if let Some(editor) = self.tasks.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(error.message.clone());
                }
                self.toast(Tone::Error, error.message);
            }
            RequestTag::TaskFire => {
                self.tasks.firing = false;
                if let Some(editor) = self.tasks.editor.as_mut()
                    && let Some(dialog) = editor.run_now.as_mut()
                {
                    dialog.sending = false;
                    dialog.error = Some(error.message.clone());
                }
                self.toast(Tone::Error, error.message);
            }
            RequestTag::TaskCancel { run_id, .. } => {
                self.runs.cancelling.remove(run_id);
                self.toast(Tone::Error, error.message.clone());
            }
            RequestTag::TaskRunFeed { generation, .. } if *generation == self.runs.generation => {
                self.runs.loading = false;
                self.runs.loading_older = false;
                self.runs.loading_newer = false;
                self.runs.error = Some(error.message.clone());
            }
            RequestTag::TaskHistoryPage { generation, .. } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if *generation != dialog.generation {
                    return;
                }
                dialog.loading = false;
                dialog.loading_older = false;
                dialog.loading_newer = false;
                dialog.error = Some(error.message.clone());
            }
            RequestTag::TaskHistoryRunGet { task_id, run_id } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if dialog.task_id != *task_id || dialog.selected != Some(*run_id) {
                    return;
                }
                dialog.detail_loading = false;
                dialog.detail_error = Some(error.message.clone());
            }
            RequestTag::TaskRunGet { task_id, run_id } => {
                if self
                    .runs
                    .selected
                    .as_ref()
                    .is_some_and(|(task, run)| task == task_id && run == run_id)
                {
                    self.runs.detail_loading = false;
                    self.runs.detail_error = Some(error.message.clone());
                }
            }
            RequestTag::SchedulesList => {
                self.schedules.loading = false;
                self.schedules.error = Some(error.message.clone());
            }
            RequestTag::ScheduleAdd | RequestTag::ScheduleUpdate => {
                if let Some(editor) = self.schedules.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(error.message.clone());
                }
                self.toast(Tone::Error, error.message);
            }
            RequestTag::ScheduleSetDefaults => {
                if let Some(editor) = self.schedules.defaults_editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(error.message.clone());
                }
                self.toast(Tone::Error, error.message);
            }
            RequestTag::SolarPreview { generation } => {
                if let Some(dialog) = self.schedules.location_dialog.as_mut()
                    && dialog.preview_generation == *generation
                {
                    dialog.previewing = false;
                    dialog.preview_error = Some(error.message.clone());
                }
            }
            RequestTag::SchedulePreview { generation } => {
                if let Some(editor) = self.schedules.editor.as_mut()
                    && editor.preview_generation == *generation
                {
                    editor.previewing = false;
                    editor.preview_error = Some(error.message.clone());
                }
            }
            RequestTag::CommandsList => {
                self.commands.loading = false;
                self.commands.error = Some(error.message.clone());
            }
            RequestTag::CommandAdd | RequestTag::CommandUpdate => {
                if let Some(editor) = self.commands.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(error.message.clone());
                }
                self.toast(Tone::Error, error.message);
            }
            _ => self.toast(Tone::Error, error.message),
        }
    }

    fn handle_timeout(&mut self, tag: RequestTag) {
        let label = tag_label(&tag);
        match &tag {
            RequestTag::MessagesPage { generation, .. }
                if *generation == self.messages.list_generation =>
            {
                self.messages.loading = false;
                self.messages.loading_older = false;
                self.messages.loading_newer = false;
                self.messages.error = Some(format!("{label} timed out"));
            }
            RequestTag::MessageSearch { generation }
                if *generation == self.messages.search_generation =>
            {
                self.messages.searching = false;
                self.messages.search_error = Some(format!("{label} timed out"));
            }
            RequestTag::MessageBatch { generation }
                if *generation == self.messages.list_generation =>
            {
                let ids = std::mem::take(&mut self.messages.batch_in_flight);
                let state_ids = std::mem::take(&mut self.messages.batch_in_flight_state_ids);
                self.messages.error = Some(format!("{label} timed out"));
                self.requeue_batch_parts(ids, state_ids);
            }
            RequestTag::MessageAnchor {
                list_generation,
                search_generation,
                ..
            } if *list_generation == self.messages.list_generation
                && *search_generation == self.messages.search_generation =>
            {
                self.messages.loading = false;
                self.messages.searching = false;
                self.messages.search_error = Some(format!("{label} timed out"));
            }
            RequestTag::MessageGet { id } => {
                self.finish_message_fetch(*id, Err(format!("{label} timed out")));
            }
            RequestTag::MessagesPage { .. }
            | RequestTag::MessageSearch { .. }
            | RequestTag::MessageBatch { .. }
            | RequestTag::MessageAnchor { .. } => (),
            RequestTag::ConfigUpdate => {
                self.config_ui.saving = false;
                self.config_ui.error = Some(format!("{label} timed out; the outcome is unknown"));
                self.refresh_config();
            }
            RequestTag::ServiceParams { .. } => {
                if let Some(Dialog::ServiceParams(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(format!("{label} timed out; the outcome is unknown"));
                }
                self.refresh_services();
                self.refresh_config();
            }
            RequestTag::ProtocolParams {
                protocol,
                transport,
            } => {
                let key = (protocol.clone(), transport.clone());
                self.protocol_params_requested.remove(&key);
                self.protocol_params_errors
                    .insert(key, format!("{label} timed out"));
            }
            RequestTag::ServiceAdd => {
                if let Some(Dialog::AddService(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(format!("{label} timed out; the outcome is unknown"));
                }
                self.refresh_services();
            }
            RequestTag::ServiceAlias => {
                if let Some(Dialog::EditAlias(dialog)) = &mut self.dialog {
                    dialog.pending = false;
                    dialog.error = Some(format!("{label} timed out; the outcome is unknown"));
                }
                self.refresh_services();
            }
            RequestTag::RawConsole => {
                self.console.pending = false;
                self.console.error = Some(format!("{label} timed out"));
                self.finish_console(ConsoleOutcome::Timeout);
            }
            RequestTag::DiscoveryStart | RequestTag::DiscoveryGet => {
                self.discovery.starting = false;
                self.discovery.error = Some(format!("{label} timed out"));
                self.discovery_poll_at = None;
            }
            RequestTag::TasksList => {
                self.tasks.loading = false;
                self.tasks.error = Some(format!("{label} timed out"));
            }
            RequestTag::TaskAdd | RequestTag::TaskUpdate => {
                if let Some(editor) = self.tasks.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::TaskFire => self.tasks.firing = false,
            RequestTag::TaskCancel { run_id, .. } => {
                self.runs.cancelling.remove(run_id);
                self.toast(Tone::Error, format!("{label} timed out"));
            }
            RequestTag::TaskRunFeed { generation, .. } if *generation == self.runs.generation => {
                self.runs.loading = false;
                self.runs.loading_older = false;
                self.runs.loading_newer = false;
                self.runs.error = Some(format!("{label} timed out"));
            }
            RequestTag::TaskHistoryPage { generation, .. } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if *generation != dialog.generation {
                    return;
                }
                dialog.loading = false;
                dialog.loading_older = false;
                dialog.loading_newer = false;
                dialog.error = Some(format!("{label} timed out"));
            }
            RequestTag::TaskHistoryRunGet { task_id, run_id } => {
                let Some(dialog) = &mut self.tasks.history else {
                    return;
                };
                if dialog.task_id != *task_id || dialog.selected != Some(*run_id) {
                    return;
                }
                dialog.detail_loading = false;
                dialog.detail_error = Some(format!("{label} timed out"));
            }
            RequestTag::TaskRunGet { task_id, run_id } => {
                if self
                    .runs
                    .selected
                    .as_ref()
                    .is_some_and(|(task, run)| task == task_id && run == run_id)
                {
                    self.runs.detail_loading = false;
                    self.runs.detail_error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::SchedulesList => {
                self.schedules.loading = false;
                self.schedules.error = Some(format!("{label} timed out"));
            }
            RequestTag::ScheduleAdd | RequestTag::ScheduleUpdate => {
                if let Some(editor) = self.schedules.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::ScheduleSetDefaults => {
                if let Some(editor) = self.schedules.defaults_editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::SolarPreview { generation } => {
                if let Some(dialog) = self.schedules.location_dialog.as_mut()
                    && dialog.preview_generation == *generation
                {
                    dialog.previewing = false;
                    dialog.preview_error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::SchedulePreview { .. } => {
                if let Some(editor) = self.schedules.editor.as_mut() {
                    editor.previewing = false;
                    editor.preview_error = Some(format!("{label} timed out"));
                }
            }
            RequestTag::CommandsList => {
                self.commands.loading = false;
                self.commands.error = Some(format!("{label} timed out"));
            }
            RequestTag::CommandAdd | RequestTag::CommandUpdate => {
                if let Some(editor) = self.commands.editor.as_mut() {
                    editor.saving = false;
                    editor.error = Some(format!("{label} timed out"));
                }
            }
            _ => {}
        }
        self.toast(Tone::Warning, format!("{label} timed out."));
        if tag.is_mutation() {
            self.toast(
                Tone::Warning,
                "The mutation outcome is unknown; data will be reloaded.",
            );
        }
    }

    fn handle_notification(&mut self, notification: Notification) {
        match notification {
            Notification::MessageNew(update) => {
                self.update_message_dialog(&update.message);
                self.messages.store_total = self.messages.store_total.saturating_add(1);
                self.queue_message_notification(
                    update.message.id,
                    PendingNotification::New,
                    Instant::now(),
                );
            }
            Notification::MessageDropped(update) => {
                if let Some(Dialog::Message(dialog)) = &mut self.dialog
                    && update.message_ids.contains(&dialog.id)
                {
                    dialog.message = None;
                    dialog.loading = false;
                    dialog.error = Some("the message was dropped from the store".to_string());
                }
                let dropped: HashSet<Uuid> = update.message_ids.iter().copied().collect();
                self.messages
                    .pending_notifications
                    .retain(|id, _| !dropped.contains(id));
                self.messages
                    .batch_in_flight
                    .retain(|id| !dropped.contains(id));
                self.messages
                    .batch_in_flight_state_ids
                    .retain(|id| !dropped.contains(id));
                self.messages.remove_ids(&update.message_ids);
            }
            Notification::MessageStateChanged(update) => {
                self.update_message_dialog(&update.message);
                self.queue_message_notification(
                    update.message.id,
                    PendingNotification::StateChanged,
                    Instant::now(),
                );
            }
            Notification::ServiceStateChanged(update) => {
                if let Some(service) = model::find_service_mut(&mut self.services, &update.service)
                {
                    service.status = update.status;
                    if self
                        .dialog
                        .as_ref()
                        .is_some_and(|dialog| matches!(dialog, Dialog::ServiceParams(_)))
                    {
                        self.refresh_services();
                    }
                } else {
                    self.refresh_services();
                }
            }
            Notification::DiscoveryStarted(update) => {
                self.discovery.starting = true;
                self.discovery.error = None;
                self.upsert_discovery(update.discovery);
            }
            Notification::TaskRunStarted(update) => {
                if let Some(info) = self
                    .tasks
                    .tasks
                    .iter_mut()
                    .find(|info| info.task.id == update.task_id)
                {
                    info.running = true;
                    info.running_run_id = Some(update.run_id);
                }
                self.runs.running.retain(|run| run.id != update.run_id);
                self.runs.running.insert(
                    0,
                    views::runs::RunningRun {
                        id: update.run_id,
                        task_id: update.task_id.clone(),
                        trigger: update.trigger.clone(),
                        started_at: update.started_at,
                        delayed_until: update.delayed_until,
                    },
                );
                if self.runs.at_bottom
                    && self.runs.selected.is_none()
                    && self.runs.selected_running.is_none()
                {
                    self.runs.scroll_anchor = None;
                    self.runs.scroll_to_bottom = true;
                }
            }
            Notification::TaskRunFinished(update) => {
                let summary = update.run;
                let task_id = summary.task_id.clone();
                let run_id = summary.id;
                let demo_cancelled = self.gateway.is_demo()
                    && summary.outcome == hodeco_contract::domain::RunOutcome::Cancelled;
                self.runs.running.retain(|run| run.id != run_id);
                self.runs.cancelling.remove(&run_id);
                if let Some(info) = self
                    .tasks
                    .tasks
                    .iter_mut()
                    .find(|info| info.task.id == task_id)
                {
                    info.running = false;
                    info.running_run_id = None;
                    if !demo_cancelled {
                        info.last_run = Some(summary.clone());
                    }
                }
                if !demo_cancelled && self.runs.filter_matches(&summary) {
                    if self.runs.loading {
                        self.runs.pending_feed.push(summary.clone());
                    }
                    self.runs.runs.retain(|run| run.id != run_id);
                    let index = self
                        .runs
                        .runs
                        .partition_point(|run| run.time <= summary.time);
                    self.runs.runs.insert(index, summary);
                    let cap =
                        views::runs::RUN_LIST_CAP.min(self.runs.feed_max_rows.max(1) as usize);
                    if self.runs.runs.len() > cap {
                        let excess = self.runs.runs.len() - cap;
                        let until = self.runs.runs[excess - 1]
                            .time
                            .checked_add_signed(chrono::TimeDelta::nanoseconds(1));
                        self.runs.runs.drain(0..excess);
                        self.runs.older_next = Some(MessageTimeRange {
                            from: self.runs.older_next.as_ref().and_then(|next| next.from),
                            until,
                        });
                    }
                    let was_selected_running = self.runs.selected_running == Some(run_id);
                    if self.runs.at_bottom
                        && self.runs.selected.is_none()
                        && (self.runs.selected_running.is_none() || was_selected_running)
                    {
                        self.runs.scroll_anchor = None;
                        self.runs.scroll_to_bottom = true;
                    } else {
                        self.runs.pending_new += 1;
                    }
                }
                if demo_cancelled {
                    if self.runs.selected_running == Some(run_id)
                        || self
                            .runs
                            .selected
                            .as_ref()
                            .is_some_and(|(task, run)| task == &task_id && *run == run_id)
                    {
                        views::runs::clear_run_selection(self);
                    }
                } else if self.runs.selected_running == Some(run_id) {
                    self.runs.selected_running = None;
                    self.select_run(&task_id, run_id);
                } else if self
                    .runs
                    .selected
                    .as_ref()
                    .is_some_and(|(task, run)| task == &task_id && *run == run_id)
                {
                    self.select_run(&task_id, run_id);
                }
                self.refresh_tasks();
            }
            Notification::TaskRunDropped(update) => {
                let dropped: std::collections::HashSet<Uuid> =
                    update.run_ids.iter().copied().collect();
                self.runs.runs.retain(|run| !dropped.contains(&run.id));
                self.runs
                    .pending_feed
                    .retain(|run| !dropped.contains(&run.id));
                if self
                    .runs
                    .selected
                    .as_ref()
                    .is_some_and(|(_, run)| dropped.contains(run))
                {
                    views::runs::clear_run_selection(self);
                }
                if self.runs.loading || self.runs.loading_older || self.runs.loading_newer {
                    self.runs.pending_dropped.extend(dropped);
                }
                self.runs.selected_index = self.runs.selected.as_ref().and_then(|(task, run)| {
                    self.runs
                        .runs
                        .iter()
                        .position(|candidate| candidate.task_id == *task && candidate.id == *run)
                });
            }
            Notification::DiscoveryFinished(update) => {
                let state = update.discovery.state;
                self.upsert_discovery(update.discovery);
                match state {
                    DiscoveryState::Failed => self.toast(Tone::Error, "Discovery failed."),
                    DiscoveryState::Cancelled => {}
                    _ => self.toast(Tone::Success, "Discovery finished."),
                }
            }
        }
    }

    fn dialog_error(&mut self, error: String) {
        match &mut self.dialog {
            Some(Dialog::AddService(dialog)) => {
                dialog.pending = false;
                dialog.error = Some(error);
            }
            Some(Dialog::EditAlias(dialog)) => {
                dialog.pending = false;
                dialog.error = Some(error);
            }
            _ => self.toast(Tone::Error, error),
        }
    }

    // -- shell -------------------------------------------------------------

    fn apply_theme_if_needed(&mut self, ctx: &Context) {
        if self.applied_theme != Some(self.theme) {
            self.theme.apply(ctx);
            self.applied_theme = Some(self.theme);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        let pressed = ctx.input(|input| {
            Tab::ALL
                .into_iter()
                .find(|tab| input.modifiers.command && input.key_pressed(tab.shortcut()))
        });
        if let Some(tab) = pressed {
            self.request_tab(tab);
        }
    }

    fn ui_top_bar(&mut self, ui: &mut Ui) {
        let theme = self.theme;
        let palette = palette(theme);
        egui::Frame::new()
            .fill(palette.panel)
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("hodeco")
                            .size(20.0)
                            .strong()
                            .color(palette.green),
                    );
                    ui.add_space(10.0);
                    for tab in Tab::ALL {
                        if ui
                            .add(egui::Button::selectable(self.tab == tab, tab.label()))
                            .clicked()
                        {
                            self.request_tab(tab);
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let theme_label = match self.theme {
                            UiTheme::Dark => "Light",
                            UiTheme::Light => "Dark",
                        };
                        if ui
                            .button(theme_label)
                            .on_hover_text("Switch theme")
                            .clicked()
                        {
                            self.theme = self.theme.other();
                        }
                        let color = crate::theme::phase_color(self.theme, self.gateway.phase());
                        let label = if self.gateway.is_demo() {
                            format!("Demo | {}", self.gateway.phase().label())
                        } else {
                            self.gateway.phase().label().to_string()
                        };
                        let response = ui
                            .horizontal(|ui| {
                                widgets::status_dot(ui, color);
                                ui.selectable_label(false, RichText::new(label).color(color))
                            })
                            .inner;
                        if response.clicked() {
                            self.open_connect_dialog();
                        }
                        response.on_hover_text("Change connection");
                    });
                });
            });
    }

    fn ui_status_bar(&mut self, ui: &mut Ui) {
        let palette = self.palette();
        egui::Frame::new()
            .fill(palette.panel)
            .inner_margin(Margin::symmetric(12, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let item = |ui: &mut Ui, text: String| {
                        ui.label(RichText::new(text).size(13.5).color(palette.muted));
                        ui.label(RichText::new("·").size(13.5).color(palette.border));
                    };
                    if self.gateway.is_demo() {
                        item(ui, "demo (in-process)".to_string());
                    } else {
                        item(ui, self.gateway.endpoint().to_string());
                    }
                    item(ui, self.gateway.phase().label().to_string());
                    item(ui, format!("{} services", self.services.len()));
                    item(ui, format!("{} messages", self.messages.store_total));
                    if let Some(server) = &self.server {
                        let version =
                            if server.version == format!("{}-demo", env!("CARGO_PKG_VERSION")) {
                                "hodeco (demo)".to_string()
                            } else {
                                format!("hodeco {}", server.version)
                            };
                        item(ui, version);
                    }
                    if let Some(last_sync) = self.last_sync {
                        ui.label(
                            RichText::new(format!("synced {}", model::format_time(last_sync)))
                                .size(13.5)
                                .color(palette.muted),
                        );
                    }
                });
            });
    }

    fn ui_banners(&mut self, ui: &mut Ui) {
        let theme = self.theme;
        if self.restarting {
            widgets::banner(
                ui,
                theme,
                Tone::Warning,
                "Restarting",
                Some("Waiting for the gateway to come back. The UI will reconnect automatically."),
                |_ui| {},
            );
        }
        match self.gateway.phase() {
            Phase::Connecting => {
                widgets::banner(
                    ui,
                    theme,
                    Tone::Info,
                    "Connecting",
                    Some(self.gateway.endpoint()),
                    |_ui| {},
                );
            }
            Phase::Syncing => {
                widgets::banner(
                    ui,
                    theme,
                    Tone::Info,
                    "Syncing",
                    Some(
                        "Subscribed to updates; loading catalogs, services, configuration, and messages.",
                    ),
                    |_ui| {},
                );
            }
            Phase::Backoff => {
                let reason = self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "connection lost".to_string());
                widgets::banner(
                    ui,
                    theme,
                    Tone::Error,
                    "Reconnecting",
                    Some(&format!(
                        "{reason}. Retrying automatically; data shown may be stale."
                    )),
                    |ui| {
                        if ui.button("Retry now").clicked() {
                            self.gateway.retry_now(Instant::now());
                        }
                        if ui.button("Change endpoint").clicked() {
                            self.open_connect_dialog();
                        }
                    },
                );
            }
            Phase::Disconnected => {
                widgets::banner(
                    ui,
                    theme,
                    Tone::Warning,
                    "Disconnected",
                    Some("The UI is not connected to a gateway."),
                    |ui| {
                        if ui.button("Retry now").clicked() {
                            self.gateway.retry_now(Instant::now());
                        }
                        if ui.button("Change endpoint").clicked() {
                            self.open_connect_dialog();
                        }
                    },
                );
            }
            Phase::Ready => {}
        }
        if self.protocol_mismatch {
            let versions = self
                .server
                .as_ref()
                .map(|server| server.protocol_version.as_str())
                .unwrap_or("unknown");
            widgets::banner(
                ui,
                theme,
                Tone::Error,
                "Protocol version mismatch",
                Some(&format!(
                    "The gateway speaks protocol {versions}; this UI expects {EXPECTED_PROTOCOL_VERSION}. Some operations may fail."
                )),
                |_ui| {},
            );
        }
        if self.stale && !matches!(self.gateway.phase(), Phase::Backoff | Phase::Disconnected) {
            widgets::banner(
                ui,
                theme,
                Tone::Warning,
                "Stale data",
                Some(
                    "The last sync failed or the connection changed. Refresh when the gateway is ready.",
                ),
                |ui| {
                    if ui.button("Refresh all").clicked() {
                        self.refresh_services();
                        self.refresh_config();
                        self.refresh_messages();
                    }
                },
            );
        }
    }

    fn ui_dialogs(&mut self, ctx: &Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut keep_open = true;
        views::dialogs::show(self, ctx, &mut dialog, &mut keep_open);
        if keep_open {
            self.dialog = Some(dialog);
        } else {
            self.dialog = None;
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &Context, _frame: &mut Frame) {
        let now = Instant::now();
        self.now = Utc::now();

        let events = self.gateway.poll(now, &self.wake);
        for event in events {
            self.reduce(event, now);
        }

        if self.gateway.phase() == Phase::Ready
            && let Some(at) = self.discovery_poll_at
            && now >= at
        {
            if let Some(discovery) = &self.discovery.current
                && discovery.state == DiscoveryState::Running
            {
                let id = discovery.id;
                self.discovery_poll_at = Some(now + Duration::from_secs(1));
                self.rpc(
                    RequestTag::DiscoveryGet,
                    Method::DiscoveryGet,
                    json!({"discoveryId": id}),
                );
            } else {
                self.discovery_poll_at = None;
            }
        }

        self.toasts
            .retain(|entry| now.saturating_duration_since(entry.created) < Duration::from_secs(7));

        let mut delay = self
            .gateway
            .next_repaint(now)
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1));
        if let Some(due) = self.messages.notification_due_at {
            delay = delay.min(due.saturating_duration_since(now));
        }
        ctx.request_repaint_after(delay);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut Frame) {
        #[cfg(target_arch = "wasm32")]
        crate::FIND_SHORTCUT_ACTIVE.store(
            self.tab == Tab::Messages && self.dialog.is_none(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.apply_theme_if_needed(ui.ctx());
        self.handle_shortcuts(ui.ctx());
        self.poll_message_requests(Instant::now());

        egui::Panel::top("hodeco-topbar").show(ui, |ui| self.ui_top_bar(ui));
        egui::Panel::bottom("hodeco-statusbar").show(ui, |ui| self.ui_status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_banners(ui);
            match self.tab {
                Tab::Services => self.ui_services_area(ui),
                Tab::Messages => self.ui_messages(ui),
                Tab::Tasks => self.ui_tasks_area(ui),
                Tab::Configuration => self.ui_configuration(ui),
                Tab::Server => self.ui_server(ui),
                Tab::Console => self.ui_console(ui),
            }
        });

        self.ui_dialogs(ui.ctx());
        let toasts: Vec<Toast> = self
            .toasts
            .iter()
            .map(|entry| entry.toast.clone())
            .collect();
        widgets::show_toasts(ui.ctx(), self.theme, &toasts);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn intersect_values<T: PartialEq + Clone>(left: &[T], right: &[T]) -> (Vec<T>, bool) {
    if left.is_empty() {
        return (right.to_vec(), false);
    }
    if right.is_empty() {
        return (left.to_vec(), false);
    }
    let intersection: Vec<T> = left
        .iter()
        .filter(|value| right.contains(value))
        .cloned()
        .collect();
    let conflict = intersection.is_empty();
    (intersection, conflict)
}

fn intersect_services(left: &[ServiceRef], right: &[ServiceRef]) -> (Vec<ServiceRef>, bool) {
    if left.is_empty() {
        return (right.to_vec(), false);
    }
    if right.is_empty() {
        return (left.to_vec(), false);
    }
    let intersection: Vec<ServiceRef> = left
        .iter()
        .filter(|value| {
            right
                .iter()
                .any(|candidate| model::refs_match(value, candidate))
        })
        .cloned()
        .collect();
    let conflict = intersection.is_empty();
    (intersection, conflict)
}

fn combined_search_filters(
    filters: &MessageFilters,
    options: &MessageFilters,
) -> Result<MessageFilters, String> {
    let (states, state_conflict) = intersect_values(&filters.states, &options.states);
    let (directions, direction_conflict) =
        intersect_values(&filters.directions, &options.directions);
    let (protocols, protocol_conflict) = intersect_values(&filters.protocols, &options.protocols);
    let (transports, transport_conflict) =
        intersect_values(&filters.transports, &options.transports);
    let (services, service_conflict) = intersect_services(&filters.services, &options.services);
    if state_conflict
        || direction_conflict
        || protocol_conflict
        || transport_conflict
        || service_conflict
    {
        return Err(
            "No message can match both the current Filters and the search Options.".to_string(),
        );
    }
    Ok(MessageFilters {
        states,
        directions,
        protocols,
        transports,
        services,
        payload_conditions: filters.payload_conditions.clone(),
    })
}

fn no_match_message(from_selection: bool, direction: SearchDirection) -> String {
    if !from_selection {
        return "No match in the filtered messages.".to_string();
    }
    match direction {
        SearchDirection::Next => {
            "No match after the current message. Clear the selection to search from the beginning."
                .to_string()
        }
        SearchDirection::Previous => {
            "No match before the current message. Clear the selection to search from the end."
                .to_string()
        }
    }
}

fn parse_console_params(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str(text) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err("params must be a JSON object".to_string()),
        Err(error) => Err(format!("invalid JSON: {error}")),
    }
}

fn parse_result<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("invalid response payload: {error}"))
}

/// Replaces the row with the same ID in `items` (when present) with `incoming`
/// and keeps the list sorted by display name.
fn replace_sorted<T>(
    items: &mut Vec<T>,
    incoming: T,
    id: impl Fn(&T) -> &str,
    name: impl Fn(&T) -> &str,
) {
    items.retain(|item| id(item) != id(&incoming));
    items.push(incoming);
    items.sort_by(|left, right| name(left).cmp(name(right)));
}

pub(crate) fn tag_label(tag: &RequestTag) -> String {
    match tag {
        RequestTag::Subscribe => "subscription".to_string(),
        RequestTag::MessageSearch { .. } => "messages.list (find)".to_string(),
        RequestTag::MessageBatch { .. } => "messages.list (notifications)".to_string(),
        RequestTag::MessageAnchor { .. } => "messages.list (anchor)".to_string(),
        RequestTag::RawConsole => "console request".to_string(),
        other => other.method().as_str().to_string(),
    }
}

pub(crate) fn service_status_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Stopped => "Stopped",
        ServiceState::Starting => "Starting",
        ServiceState::Running => "Running",
        ServiceState::Error => "Error",
    }
}

pub(crate) fn message_states() -> &'static [MessageState] {
    &MESSAGE_STATES
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_codec::filter::{FrameKind, Includes};

    fn message(index: u8) -> Message {
        Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![index]),
        }
    }

    #[test]
    fn local_pages_keep_oldest_first_order() {
        let mut messages = MessagesUi::default();
        let first = message(1);
        let second = message(2);
        let older = MessageTimeRange {
            from: None,
            until: Some(first.time),
        };
        messages.replace_page(vec![second.clone(), first.clone()], Some(older.clone()), 20);
        assert_eq!(
            messages
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
        assert_eq!(messages.older_next, Some(older));
        assert_eq!(messages.store_total, 20);
    }

    #[test]
    fn updates_and_drops_messages_by_id() {
        let mut messages = MessagesUi::default();
        let first = message(1);
        let second = message(2);
        messages.replace_page(vec![first.clone(), second.clone()], None, 2);
        messages.selected = Some(first.id);
        messages.selected_message = Some(first.clone());

        let mut updated = first.clone();
        updated.state = MessageState::Outgoing;
        messages.upsert_messages(&[updated.clone()]);
        assert_eq!(messages.selected_message.as_ref(), Some(&updated));

        messages.remove_ids(&[first.id]);
        assert_eq!(messages.messages.len(), 1);
        assert!(messages.selected.is_none());
        assert_eq!(messages.store_total, 1);
    }

    #[test]
    fn pages_store_the_gateway_next_bounds() {
        let mut messages = MessagesUi::default();
        let first = message(1);
        let second = message(2);
        let older = MessageTimeRange {
            from: None,
            until: Some(first.time),
        };
        let older_again = MessageTimeRange {
            from: None,
            until: Some(first.time),
        };
        let newer = MessageTimeRange {
            from: Some(
                second
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
                    .unwrap(),
            ),
            until: None,
        };

        messages.replace_page(vec![first.clone(), second.clone()], Some(older), 2);
        messages.prepend_older(vec![first.clone()], Some(older_again.clone()), 3);
        assert_eq!(messages.older_next, Some(older_again));

        messages.append_newer(vec![second.clone()], Some(newer.clone()), 4);
        assert_eq!(messages.newer_next, Some(newer));
    }

    #[test]
    fn paging_bounds_follow_removed_edges() {
        let mut messages = MessagesUi::default();
        let first = message(1);
        let second = message(2);
        messages.replace_page(vec![first.clone(), second.clone()], None, 2);
        messages.older_next = Some(MessageTimeRange {
            from: None,
            until: Some(first.time),
        });
        messages.newer_next = Some(MessageTimeRange {
            from: Some(
                second
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
                    .unwrap(),
            ),
            until: None,
        });

        messages.remove_ids(&[first.id]);
        assert_eq!(
            messages.older_next,
            Some(MessageTimeRange {
                from: None,
                until: Some(second.time),
            })
        );

        messages.remove_ids(&[second.id]);
        assert_eq!(messages.older_next, None);
        assert_eq!(messages.newer_next, None);
    }

    #[test]
    fn filter_drafts_compile_frame_and_raw_conditions() {
        let frame = filter::FilterSpec {
            frame: FrameKind::SendData,
            nodes: vec![2],
            includes: Includes {
                node: true,
                frame_type: true,
                serial_command: true,
                ..Includes::default()
            },
            ..filter::FilterSpec::default()
        };
        let raw = RawConditionDraft {
            leaves: vec![RawLeafDraft {
                anywhere: true,
                bytes: "01 02".to_string(),
                mask: "ff".to_string(),
                ..RawLeafDraft::default()
            }],
        };
        let zigbee = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_PERMIT_JOINING),
            params: "b4".to_string(),
            ..ZigbeeConditionDraft::default()
        };
        let draft = FilterDraft {
            payload_conditions: vec![
                PayloadConditionDraft::Frame(frame),
                PayloadConditionDraft::Raw(raw),
                PayloadConditionDraft::Zigbee(zigbee),
            ],
            ..FilterDraft::default()
        };
        let expression = draft.payload_expr().unwrap().expect("expression");
        assert!(expression.matches(&[0x01, 0x13, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]));
        assert!(expression.matches(&[9, 1, 2]));
        assert!(expression.matches(&[0, 0, 1, 0x22, 0, 0xb4]));
        assert_eq!(draft.leaf_count(), 4);
    }

    #[test]
    fn zigbee_condition_capabilities_follow_the_command() {
        let mut draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_INCOMING_MESSAGE_HANDLER),
            ..ZigbeeConditionDraft::default()
        };
        assert!(draft.supports_network());
        assert!(draft.supports_group());
        assert!(draft.supports_cluster());
        assert!(draft.supports_endpoint());
        assert!(draft.supports_zcl());

        draft.command = Some(zigbee::EZSP_PERMIT_JOINING);
        assert!(!draft.supports_network());
        assert!(!draft.supports_group());
        assert!(!draft.supports_cluster());
        assert!(!draft.supports_endpoint());
        assert!(!draft.supports_zcl());

        draft.command = Some(zigbee::EZSP_SEND_BROADCAST);
        assert!(draft.supports_network());
        assert!(draft.supports_cluster());
        assert!(draft.supports_endpoint());
        assert!(draft.supports_zcl());

        draft.command = Some(zigbee::EZSP_SEND_MULTICAST);
        assert!(!draft.supports_network());
        assert!(draft.supports_cluster());
        assert!(draft.supports_endpoint());
        assert!(draft.supports_zcl());
    }

    #[test]
    fn zigbee_condition_matches_extra_ezsp_fields() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "duration".to_string(),
            ZigbeeFieldMatch {
                enabled: true,
                value: "180".to_string(),
            },
        );
        let draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_PERMIT_JOINING),
            ezsp_fields: fields,
            ..ZigbeeConditionDraft::default()
        };
        let expression = draft.to_expr().unwrap();
        assert!(expression.matches(&zigbee::build_request(
            0,
            zigbee::EZSP_PERMIT_JOINING,
            &[180]
        )));
    }

    #[test]
    fn zigbee_extra_fields_follow_command_layout() {
        let draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_SEND_UNICAST),
            ..ZigbeeConditionDraft::default()
        };
        let names: Vec<String> = draft
            .extra_field_candidates()
            .into_iter()
            .map(|field| field.name)
            .collect();
        assert!(names.contains(&"profileId".to_string()));
        assert!(names.contains(&"apsOptions".to_string()));
        assert!(!names.contains(&"clusterId".to_string()));
        assert!(!names.contains(&"messageContents".to_string()));
        assert!(!names.contains(&"status".to_string()));

        let response_draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_SEND_UNICAST),
            ezsp_response: true,
            ..ZigbeeConditionDraft::default()
        };
        let names: Vec<String> = response_draft
            .extra_field_candidates()
            .into_iter()
            .map(|field| field.name)
            .collect();
        assert!(names.contains(&"status".to_string()));
        assert!(names.contains(&"sequence".to_string()));
        assert!(!names.contains(&"profileId".to_string()));

        let response_draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_NETWORK_STATE),
            ..ZigbeeConditionDraft::default()
        };
        let names: Vec<String> = response_draft
            .extra_field_candidates()
            .into_iter()
            .map(|field| field.name)
            .collect();
        assert_eq!(names, vec!["status".to_string()]);
    }

    #[test]
    fn zigbee_condition_matches_multicast_group() {
        let draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_SEND_MULTICAST),
            group: "0001".to_string(),
            cluster: "0006".to_string(),
            ..ZigbeeConditionDraft::default()
        };
        let expression = draft.to_expr().unwrap();
        let zcl = [0x01, 0x01, 0x01];
        let mut params = vec![
            0x04,
            0x01,
            0x06,
            0x00,
            0x01,
            0x01,
            0x00,
            0x00,
            0x01,
            0x00,
            0x01,
            0x01,
            0x01,
            0x01,
            zcl.len() as u8,
        ];
        params.extend(zcl);
        let frame = zigbee::build_request(4, zigbee::EZSP_SEND_MULTICAST, &params);
        assert!(expression.matches(&frame));
    }

    #[test]
    fn zigbee_condition_matches_typed_zcl_fields() {
        let mut zcl_fields = BTreeMap::new();
        zcl_fields.insert(
            "level".to_string(),
            ZigbeeFieldMatch {
                enabled: true,
                value: "128".to_string(),
            },
        );
        zcl_fields.insert(
            "transitionTime".to_string(),
            ZigbeeFieldMatch {
                enabled: true,
                value: "2".to_string(),
            },
        );
        let draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_SEND_UNICAST),
            network: "1234".to_string(),
            cluster: "0008".to_string(),
            endpoint: "01".to_string(),
            zcl_command: Some(0x00),
            zcl_fields,
            ..ZigbeeConditionDraft::default()
        };
        let expression = draft.to_expr().unwrap();
        let zcl = [0x01, 0x00, 0x00, 0x80, 0x02, 0x00];
        let mut params = vec![
            0x00,
            0x34,
            0x12,
            0x04,
            0x01,
            0x08,
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
        let frame = zigbee::build_request(7, zigbee::EZSP_SEND_UNICAST, &params);
        assert!(expression.matches(&frame));

        let mut wrong = frame.clone();
        wrong[24] = 100;
        assert!(!expression.matches(&wrong));
    }

    #[test]
    fn zigbee_condition_matches_general_zcl_command() {
        let draft = ZigbeeConditionDraft {
            command: Some(zigbee::EZSP_SEND_UNICAST),
            network: "1234".to_string(),
            cluster: "0006".to_string(),
            endpoint: "01".to_string(),
            zcl_general: true,
            zcl_command: Some(0x0a),
            zcl_direction_to_client: true,
            zcl_payload: "00 01 00".to_string(),
            ..ZigbeeConditionDraft::default()
        };
        let expression = draft.to_expr().unwrap();
        let zcl = [0x08, 0x00, 0x0a, 0x00, 0x01, 0x00];
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
        let frame = zigbee::build_request(7, zigbee::EZSP_SEND_UNICAST, &params);
        assert!(expression.matches(&frame));
        let wrong = ZigbeeConditionDraft {
            zcl_direction_to_client: false,
            ..draft
        };
        assert!(wrong.to_expr().is_err());
    }

    #[test]
    fn empty_console_params_mean_an_empty_object() {
        assert_eq!(parse_console_params("").unwrap(), json!({}));
        assert_eq!(parse_console_params("  \n ").unwrap(), json!({}));
        assert_eq!(
            parse_console_params(" {\"limit\": 1} ").unwrap(),
            json!({"limit": 1})
        );
        assert!(parse_console_params("[]").is_err());
        assert!(parse_console_params("{").is_err());
    }

    #[test]
    fn reconnect_reset_preserves_filters_and_search_text() {
        let mut messages = MessagesUi::default();
        messages.filters.states = vec![MessageState::Incoming];
        messages.options.protocols = vec!["zwave".to_string()];
        messages.search = "hodeco".to_string();
        messages.search_mode = model::PayloadMode::Binary;
        messages.reset();
        assert_eq!(messages.filters.states, vec![MessageState::Incoming]);
        assert_eq!(messages.options.protocols, vec!["zwave".to_string()]);
        assert_eq!(messages.search, "hodeco");
        assert_eq!(messages.search_mode, model::PayloadMode::Binary);
        assert!(messages.messages.is_empty());
        assert!(messages.loading);
    }

    #[test]
    fn metadata_intersection_and_no_match_messages() {
        let filters = MessageFilters {
            states: vec![MessageState::Incoming, MessageState::Outgoing],
            ..Default::default()
        };
        let options = MessageFilters {
            states: vec![MessageState::Incoming],
            ..Default::default()
        };
        let combined = combined_search_filters(&filters, &options).unwrap();
        assert_eq!(combined.states, vec![MessageState::Incoming]);

        let conflict = combined_search_filters(
            &filters,
            &MessageFilters {
                states: vec![MessageState::OutgoingTimedOut],
                ..Default::default()
            },
        );
        assert!(conflict.is_err());
        assert!(no_match_message(true, SearchDirection::Next).contains("after"));
        assert!(no_match_message(true, SearchDirection::Previous).contains("before"));
        assert_eq!(
            no_match_message(false, SearchDirection::Next),
            "No match in the filtered messages."
        );
    }

    struct DemoUi {
        ctx: egui::Context,
        app: App,
        frame: Frame,
    }

    impl DemoUi {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let creation = eframe::CreationContext::_new_kittest(ctx.clone());
            let app = App::new(
                &creation,
                Startup {
                    endpoint: String::new(),
                    demo: true,
                },
            );
            Self {
                ctx,
                app,
                frame: eframe::Frame::_new_kittest(),
            }
        }

        fn run(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            use eframe::App as _;

            self.app.logic(&self.ctx, &mut self.frame);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            };
            let mut output = self
                .ctx
                .run_ui(input, |ui| self.app.ui(ui, &mut self.frame));
            output.textures_delta.clear();
            output
        }

        fn click(&mut self, pos: egui::Pos2) {
            let _ = self.run(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            let _ = self.run(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }]);
        }

        fn wait_until_ready(&mut self) {
            for _ in 0..200 {
                let _ = self.run(Vec::new());
                if self.app.gateway.phase() == Phase::Ready
                    && !self.app.protocols.is_empty()
                    && !self.app.transports.is_empty()
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!(
                "demo gateway did not become ready: phase={:?}, protocols={}, transports={}",
                self.app.gateway.phase(),
                self.app.protocols.len(),
                self.app.transports.len()
            );
        }

        fn wait_for_params(&mut self, key: &(String, String)) {
            for _ in 0..50 {
                let _ = self.run(Vec::new());
                if self.app.protocol_params.contains_key(key)
                    || self.app.protocol_params_errors.contains_key(key)
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                self.app.protocol_params.contains_key(key),
                "parameter definitions did not load: {:?}",
                self.app.protocol_params_errors.get(key)
            );
        }

        fn click_dialog_submit(&mut self) {
            let output = self.run(Vec::new());
            let mut buttons = text_rects(&output, "Add service");
            buttons.sort_by(|a, b| a.center().y.total_cmp(&b.center().y));
            let submit = buttons.last().expect("dialog submit button");
            self.click(submit.center());
        }

        /// Runs frames until `text` keeps the same position between two frames.
        fn stable_text_rect(&mut self, text: &str) -> egui::Rect {
            let mut previous = None;
            for _ in 0..40 {
                let output = self.run(Vec::new());
                let rect = text_rect(&output, text);
                if previous.is_some_and(|previous: egui::Rect| {
                    (previous.center() - rect.center()).length() < 0.1
                }) {
                    return rect;
                }
                previous = Some(rect);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            previous.expect("text was never painted")
        }

        /// Submits the open Add service dialog, waiting for its parameter
        /// definitions first, and asserts that the service appears.
        fn submit_add_service(&mut self, entry: &str) {
            let Some(Dialog::AddService(dialog)) = &mut self.app.dialog else {
                panic!("the Add service dialog is not open");
            };
            assert_eq!(dialog.entry, entry);
            let key = (dialog.protocol.clone(), dialog.transport.clone());

            self.wait_for_params(&key);
            self.click_dialog_submit();

            for _ in 0..50 {
                let _ = self.run(Vec::new());
                if self
                    .app
                    .services
                    .iter()
                    .any(|service| service.entry == entry)
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the service was not added; dialog = {:?}", self.app.dialog);
        }
    }

    fn text_rects(output: &egui::FullOutput, text: &str) -> Vec<egui::Rect> {
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(shape) if shape.galley.text() == text => {
                    Some(clipped.shape.visual_bounding_rect())
                }
                _ => None,
            })
            .collect()
    }

    fn text_rect(output: &egui::FullOutput, text: &str) -> egui::Rect {
        text_rects(output, text)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no text shape {text:?}"))
    }

    #[test]
    fn zigbee_filter_falls_back_to_all_zigbee_services() {
        let mut demo = DemoUi::new();
        demo.wait_until_ready();

        let services = views::messages::zigbee_filter_services(&demo.app, &[]);
        assert!(!services.is_empty(), "no ZigBee service found");
        let keys: Vec<String> = services
            .iter()
            .flat_map(|service| model::entity_service_keys(&demo.app.services, service))
            .collect();
        let devices = views::messages::zigbee_known_devices(&keys);
        assert!(
            devices.iter().any(|(nwk, _, _)| *nwk == 0x1234),
            "known devices were empty: {devices:?}"
        );
    }

    #[test]
    fn zwave_filter_known_nodes_fall_back_to_all_zwave_services() {
        let mut demo = DemoUi::new();
        demo.wait_until_ready();

        let nodes = views::messages::zwave_known_nodes(&demo.app, &[]);
        assert!(
            nodes.iter().any(|(node, _)| *node == 3),
            "known nodes were empty: {nodes:?}"
        );
    }

    #[test]
    fn demo_services_add_service_opens_the_dialog() {
        let mut demo = DemoUi::new();
        demo.wait_until_ready();

        let output = demo.run(Vec::new());
        demo.click(text_rect(&output, "Add service").center());

        assert!(
            matches!(demo.app.dialog, Some(Dialog::AddService(_))),
            "the Add service dialog did not open"
        );

        let _ = demo.run(vec![egui::Event::PointerMoved(egui::pos2(4.0, 4.0))]);
        assert!(
            matches!(demo.app.dialog, Some(Dialog::AddService(_))),
            "the Add service dialog closed immediately"
        );
    }

    #[test]
    fn demo_services_add_service_dialog_adds_a_service() {
        let mut demo = DemoUi::new();
        demo.wait_until_ready();

        let output = demo.run(Vec::new());
        demo.click(text_rect(&output, "Add service").center());
        let Some(Dialog::AddService(dialog)) = &mut demo.app.dialog else {
            panic!("the Add service dialog did not open");
        };
        dialog.entry = "COM6".to_string();
        demo.submit_add_service("COM6");
    }

    #[test]
    fn demo_add_service_failure_is_visible_in_the_footer() {
        let mut demo = DemoUi::new();
        demo.wait_until_ready();

        let output = demo.run(Vec::new());
        demo.click(text_rect(&output, "Add service").center());
        let Some(Dialog::AddService(dialog)) = &mut demo.app.dialog else {
            panic!("the Add service dialog did not open");
        };
        // COM4 is already configured in the demo fixtures.
        dialog.entry = "COM4".to_string();
        let key = (dialog.protocol.clone(), dialog.transport.clone());

        demo.wait_for_params(&key);
        let cancel_before = demo.stable_text_rect("Cancel");
        demo.click_dialog_submit();

        let mut previous = None;
        for _ in 0..80 {
            let output = demo.run(Vec::new());
            let Some(Dialog::AddService(dialog)) = &demo.app.dialog else {
                panic!("the Add service dialog closed after a failed add");
            };
            let Some(error) = &dialog.error else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            let cancel = text_rect(&output, "Cancel");
            let stable = previous.is_some_and(|previous: egui::Rect| {
                (previous.center() - cancel.center()).length() < 0.1
            });
            previous = Some(cancel);
            if !stable {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            let footer_error = text_rects(&output, error).into_iter().find(|rect| {
                rect.top() < cancel.bottom()
                    && rect.bottom() > cancel.top()
                    && rect.right() <= cancel.left() + 1.0
            });
            assert!(
                footer_error.is_some(),
                "the add error was not painted beside the footer actions: {error:?}"
            );
            assert!(
                (cancel_before.center() - cancel.center()).length() < 1.0,
                "the footer actions moved when the error appeared: before={cancel_before:?} after={cancel:?}"
            );
            return;
        }
        panic!("the add error did not arrive");
    }

    #[test]
    fn demo_discovery_add_service_adds_the_discovered_entry() {
        use hodeco_contract::domain::DiscoveryEntry;

        let mut demo = DemoUi::new();
        demo.wait_until_ready();
        demo.app.services_tab = ServicesTab::Discovery;
        demo.app.discovery.protocol = "zigbee".to_string();
        demo.app.discovery.transport = "serial".to_string();
        demo.app.discovery.current = Some(DiscoveryInfo {
            id: Uuid::new_v4(),
            protocol: "zigbee".to_string(),
            transport: "serial".to_string(),
            state: DiscoveryState::Finished,
            entries: vec![DiscoveryEntry {
                entry: "COM8".to_string(),
                available: true,
                description: Some("Nortec HUSBZB-1 ZigBee Hub".to_string()),
                error: None,
            }],
        });

        let output = demo.run(Vec::new());
        demo.click(text_rect(&output, "Add service").center());
        let Some(Dialog::AddService(dialog)) = &demo.app.dialog else {
            panic!("the Add service dialog did not open");
        };
        assert_eq!(dialog.entry, "COM8");
        demo.submit_add_service("COM8");
    }
}
