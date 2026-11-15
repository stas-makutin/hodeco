//! Application shell, shared state, and gateway event reduction.
//!
//! Views live in `app/views` and only read this state or call the small action
//! helpers below. All transport work is owned by [`Gateway`]; the UI never
//! speaks HTTP.

mod views;

use crate::client::endpoint;
use crate::client::{Gateway, GatewayEvent, MessagePageKind, Phase, RequestTag, Wake};
use crate::model::{
    self, EXPECTED_PROTOCOL_VERSION, MESSAGE_NOTIFICATION_BATCH, MESSAGE_PAGE_LIMIT, MESSAGE_STATES,
};
use crate::startup::Startup;
use crate::theme::{UiTheme, palette};
use crate::ui::widgets::{self, Toast, Tone};
use chrono::{DateTime, Utc};
use eframe::Frame;
use egui::{Align, Context, Layout, Margin, RichText, Ui};
use hodeco_codec::filter;
use hodeco_contract::api::notifications::Notification;
use hodeco_contract::api::payloads::{
    ConfigUpdateResult, DiscoveryGetResult, DiscoveryStartResult, MessageTimeRange,
    MessagesGetResult, MessagesListResult, ProtocolParamsResult, ProtocolsListResult,
    ServerInfoResult, ServicesAddResult, ServicesAliasResult, ServicesListResult,
    ServicesParamsResult, ServicesSendResult, ServicesStatusResult, TransportsListResult,
};
use hodeco_contract::api::{ApiError, Method};
use hodeco_contract::config::Config;
use hodeco_contract::domain::{
    DiscoveryInfo, DiscoveryState, Message, MessageDirection, MessageOrder, MessageState, ParamDef,
    ParamValues, PayloadExpr, PayloadOffset, PayloadOp, ProtocolInfo, ServiceInfo, ServiceRef,
    ServiceState, TransportInfo,
};
use hodeco_contract::utils::{Base64Bytes, HumanDuration};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use web_time::Instant;

use views::builder::{BuilderDialog, new_builder};

// ---------------------------------------------------------------------------
// Navigation and shared UI state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Services,
    Discovery,
    Messages,
    Configuration,
    Server,
    Console,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::Services,
        Self::Discovery,
        Self::Messages,
        Self::Configuration,
        Self::Server,
        Self::Console,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Discovery => "Discovery",
            Self::Messages => "Messages",
            Self::Configuration => "Configuration",
            Self::Server => "Server",
            Self::Console => "Console",
        }
    }

    pub fn shortcut(self) -> egui::Key {
        match self {
            Self::Services => egui::Key::Num1,
            Self::Discovery => egui::Key::Num2,
            Self::Messages => egui::Key::Num3,
            Self::Configuration => egui::Key::Num4,
            Self::Server => egui::Key::Num5,
            Self::Console => egui::Key::Num6,
        }
    }

    pub fn is_secondary(self) -> bool {
        matches!(self, Self::Configuration | Self::Server | Self::Console)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadConditionDraft {
    Frame(filter::FilterSpec),
    Raw(RawConditionDraft),
}

impl PayloadConditionDraft {
    pub fn summary(&self) -> String {
        match self {
            Self::Frame(spec) => spec.summary(),
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

    pub(crate) server: Option<ServerInfoResult>,
    pub(crate) protocols: Vec<ProtocolInfo>,
    pub(crate) transports: Vec<TransportInfo>,
    pub(crate) services: Vec<ServiceInfo>,
    pub(crate) config: Option<Config>,

    pub(crate) messages: MessagesUi,
    pub(crate) discovery: DiscoveryUi,
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
            server: None,
            protocols: Vec::new(),
            transports: Vec::new(),
            services: Vec::new(),
            config: None,
            messages: MessagesUi::default(),
            discovery: DiscoveryUi::default(),
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
                    let narrow = ui.ctx().content_rect().width() < 1100.0;
                    let primary = [Tab::Services, Tab::Discovery, Tab::Messages];
                    for tab in primary {
                        if ui
                            .add(egui::Button::selectable(self.tab == tab, tab.label()))
                            .clicked()
                        {
                            self.request_tab(tab);
                        }
                    }
                    if narrow {
                        let secondary_active = self.tab.is_secondary();
                        let more_text = if secondary_active {
                            RichText::new("More *").color(palette.accent)
                        } else {
                            RichText::new("More")
                        };
                        ui.menu_button(more_text, |ui| {
                            for tab in [Tab::Configuration, Tab::Server, Tab::Console] {
                                if ui
                                    .add(egui::Button::selectable(self.tab == tab, tab.label()))
                                    .clicked()
                                {
                                    self.request_tab(tab);
                                    ui.close();
                                }
                            }
                        });
                    } else {
                        for tab in [Tab::Configuration, Tab::Server, Tab::Console] {
                            if ui
                                .add(egui::Button::selectable(self.tab == tab, tab.label()))
                                .clicked()
                            {
                                self.request_tab(tab);
                            }
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
                Tab::Services => self.ui_services(ui),
                Tab::Discovery => self.ui_discovery(ui),
                Tab::Messages => self.ui_messages(ui),
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

pub(crate) fn tag_label(tag: &RequestTag) -> String {
    match tag {
        RequestTag::Subscribe => "subscription".to_string(),
        RequestTag::ServerInfo => "server.info".to_string(),
        RequestTag::ProtocolList => "protocols.list".to_string(),
        RequestTag::TransportList => "transports.list".to_string(),
        RequestTag::Config => "config.get".to_string(),
        RequestTag::ConfigUpdate => "config.update".to_string(),
        RequestTag::ServiceList => "services.list".to_string(),
        RequestTag::MessagesPage { .. } => "messages.list".to_string(),
        RequestTag::MessageSearch { .. } => "messages.list (find)".to_string(),
        RequestTag::MessageBatch { .. } => "messages.list (notifications)".to_string(),
        RequestTag::MessageAnchor { .. } => "messages.list (anchor)".to_string(),
        RequestTag::MessageGet { .. } => "messages.get".to_string(),
        RequestTag::ServiceAdd => "services.add".to_string(),
        RequestTag::ServiceRemove => "services.remove".to_string(),
        RequestTag::ServiceAlias => "services.alias".to_string(),
        RequestTag::ServiceStatus { .. } => "services.status".to_string(),
        RequestTag::ServiceParams { .. } => "services.params".to_string(),
        RequestTag::ServiceSend => "services.send".to_string(),
        RequestTag::DiscoveryStart => "discovery.start".to_string(),
        RequestTag::DiscoveryGet => "discovery.get".to_string(),
        RequestTag::DiscoveryCancel => "discovery.cancel".to_string(),
        RequestTag::ProtocolParams { .. } => "protocols.params".to_string(),
        RequestTag::RawConsole => "console request".to_string(),
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
        let draft = FilterDraft {
            payload_conditions: vec![
                PayloadConditionDraft::Frame(frame),
                PayloadConditionDraft::Raw(raw),
            ],
            ..FilterDraft::default()
        };
        let expression = draft.payload_expr().unwrap().expect("expression");
        assert!(expression.matches(&[0x01, 0x13, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]));
        assert!(expression.matches(&[9, 1, 2]));
        assert_eq!(draft.leaf_count(), 2);
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
}
