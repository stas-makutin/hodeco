//! In-process fixture server used by `--demo` and the component gallery.
//!
//! It speaks the same JSON envelopes as the real gateway and implements the
//! read-only portions of the RPC surface plus enough mutation to exercise the UI:
//! service add/remove/alias/send, discovery transitions, and live traffic.
//! Nothing here touches the network, so it also works in the browser.

use chrono::{DateTime, TimeDelta, Utc};
use hodeco_codec::zigbee;
use hodeco_codec::zwave::{request as frame_request, response as frame_response};
use hodeco_contract::api::notifications::{
    DiscoveryNotification, MessageNotification, Notification, TaskRunFinishedNotification,
};
use hodeco_contract::api::payloads::{
    CommandsAddParams, CommandsAddResult, CommandsGetParams, CommandsGetResult, CommandsListResult,
    CommandsRemoveParams, CommandsUpdateParams, CommandsUpdateResult, ConfigUpdateParams,
    ConfigUpdateResult, DiscoveryCancelParams, DiscoveryGetParams, DiscoveryGetResult,
    DiscoveryStartParams, DiscoveryStartResult, Empty, EventTopic, EventsSubscribeParams,
    EventsUnsubscribeParams, MessageTimeRange, MessagesGetParams, MessagesGetResult,
    MessagesListParams, MessagesListResult, ProtocolInfoParams, ProtocolInfoResult,
    ProtocolParamsParams, ProtocolParamsResult, ProtocolsListResult, RunFeedTotals,
    SchedulesAddParams, SchedulesAddResult, SchedulesGetParams, SchedulesGetResult,
    SchedulesListResult, SchedulesPreviewParams, SchedulesPreviewResult, SchedulesRemoveParams,
    SchedulesSetDefaultsParams, SchedulesSetDefaultsResult, SchedulesSolarPreviewParams,
    SchedulesSolarPreviewResult, SchedulesUpdateParams, SchedulesUpdateResult, ServerInfoResult,
    ServicesAddParams, ServicesAddResult, ServicesAliasParams, ServicesAliasResult,
    ServicesListResult, ServicesParamsParams, ServicesParamsResult, ServicesRemoveParams,
    ServicesSendParams, ServicesSendResult, ServicesStatusParams, ServicesStatusResult,
    SolarPreviewEvent, TaskStoreTotal, TasksAddParams, TasksAddResult, TasksCancelRunParams,
    TasksFireParams, TasksFireResult, TasksGetParams, TasksGetResult, TasksGetRunParams,
    TasksGetRunResult, TasksHistoryParams, TasksHistoryResult, TasksListResult,
    TasksPurgeHistoryParams, TasksRemoveParams, TasksRunFeedParams, TasksRunFeedResult,
    TasksUpdateParams, TasksUpdateResult, TransportsListResult,
};
use hodeco_contract::api::{ApiError, ErrorCode, Method, Request, Response};
use hodeco_contract::catalog;
use hodeco_contract::config::Config;
use hodeco_contract::domain::{
    ActionKind, ActionOutcome, AtSpec, CommandAction, CommandDefinition, CommandInfo,
    DiscoveryEntry, DiscoveryInfo, DiscoveryState, Location, ManualRunTrigger, Message,
    MessageAction, MessageActionPayload, MessageOrder, MessageState, OneOrMany, OutputMode,
    ParamDef, ParamType, ProtocolInfo, RunAction, RunChain, RunCommandAction, RunMessageAction,
    RunOutcome, RunSummary, RunTrigger, ScheduleDefinition, ScheduleInfo, ScheduleTrigger,
    ServiceInfo, ServiceRef, ServiceState, ServiceStatus, SolarEvent, TaskAction, TaskDefinition,
    TaskInfo, TaskRun, TaskTrigger, TransportInfo, TriggerRef, TriggerType,
};
use hodeco_contract::utils::{Base64Bytes, HumanDuration, id_from_name};
use hodeco_schedule::ScheduleContext;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

const DEMO_PROTOCOL_VERSION: &str = crate::model::EXPECTED_PROTOCOL_VERSION;
const DEMO_GET_VERSION: [u8; 14] = [
    0x15, b'Z', b'-', b'W', b'a', b'v', b'e', b' ', b'4', b'.', b'0', b'5', 0x00, 0x01,
];

pub struct DemoServer {
    config: Config,
    runtime_config: Config,
    services: Vec<ServiceInfo>,
    messages: Vec<Message>,
    last_time: DateTime<Utc>,
    subscribed: Vec<EventTopic>,
    discovery: Option<DiscoveryInfo>,
    discovery_finished: bool,
    pending_outgoing: Vec<Uuid>,
    started_at: DateTime<Utc>,
    ticks: u64,
    next_traffic: usize,
    tasks: Vec<TaskInfo>,
    schedules: Vec<ScheduleInfo>,
    schedule_location: Option<Location>,
    schedule_timezone: Option<String>,
    commands: Vec<CommandInfo>,
    runs: Vec<TaskRun>,
}

impl Default for DemoServer {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoServer {
    pub fn new() -> Self {
        let now = Utc::now();
        let services = vec![
            ServiceInfo {
                alias: Some("ZW".to_string()),
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM4".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Running,
                    error: None,
                    detail: None,
                },
                params: BTreeMap::from([
                    ("baudRate".to_string(), json!(115_200)),
                    ("openAttemptsInterval".to_string(), json!("3 s")),
                    ("outgoingMaxTTL".to_string(), json!("10 s")),
                ]),
            },
            ServiceInfo {
                alias: None,
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM3".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Error,
                    error: Some("access denied while opening COM3".to_string()),
                    detail: None,
                },
                params: BTreeMap::new(),
            },
            ServiceInfo {
                alias: Some("ZW-Lab".to_string()),
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM7".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Starting,
                    error: None,
                    detail: None,
                },
                params: BTreeMap::from([("baudRate".to_string(), json!(57_600))]),
            },
            ServiceInfo {
                alias: Some("HTTP".to_string()),
                protocol: "http".to_string(),
                transport: "tcp".to_string(),
                entry: "api.example.test:443".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Running,
                    error: None,
                    detail: None,
                },
                params: BTreeMap::from([
                    ("connectTimeout".to_string(), json!("10 s")),
                    ("readTimeout".to_string(), json!("30 s")),
                ]),
            },
            ServiceInfo {
                alias: Some("ZB".to_string()),
                protocol: "zigbee".to_string(),
                transport: "serial".to_string(),
                entry: "COM5".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Running,
                    error: None,
                    detail: Some("EZSP v4 · stack 0x5410".to_string()),
                },
                params: BTreeMap::from([
                    ("baudRate".to_string(), json!(57_600)),
                    ("flowControl".to_string(), json!("none")),
                    ("openAttemptsInterval".to_string(), json!("3 s")),
                    ("outgoingMaxTTL".to_string(), json!("10 s")),
                ]),
            },
        ];

        let alias = ServiceRef::Alias {
            alias: "ZW".to_string(),
        };
        let http = ServiceRef::Alias {
            alias: "HTTP".to_string(),
        };
        let zigbee = ServiceRef::Alias {
            alias: "ZB".to_string(),
        };
        let mut messages = vec![
            new_message(
                now - TimeDelta::minutes(12),
                &alias,
                MessageState::Incoming,
                vec![0x06],
            ),
            new_message(
                now - TimeDelta::minutes(11),
                &alias,
                MessageState::Incoming,
                frame_response(&DEMO_GET_VERSION),
            ),
            new_message(
                now - TimeDelta::minutes(9),
                &alias,
                MessageState::Outgoing,
                frame_request(&[0x13, 0x02, 0x03, 0x20, 0x01, 0x63, 0x00, 0x01]),
            ),
            new_message(
                now - TimeDelta::minutes(8),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x2d]),
            ),
            new_message(
                now - TimeDelta::minutes(6),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x02, 0x03, 0x25, 0x03, 0xff]),
            ),
            new_message(
                now - TimeDelta::minutes(4),
                &alias,
                MessageState::Incoming,
                frame_request(&[
                    0x04, 0x00, 0x06, 0x08, 0x32, 0x02, 0x01, 0x00, 0x00, 0x00, 0x30, 0x39,
                ]),
            ),
            new_message(
                now - TimeDelta::minutes(3),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x07, 0x04, 0x26, 0x03, 0x48, 0x05]),
            ),
            new_message(
                now - TimeDelta::minutes(2),
                &alias,
                MessageState::OutgoingTimedOut,
                frame_request(&[0x13, 0x03, 0x03, 0x20, 0x01, 0x00, 0x00, 0x02]),
            ),
            new_message(
                now - TimeDelta::minutes(1),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x04, 0x04, 0x2b, 0x01, 0x05, 0x00]),
            ),
            {
                let mut payload = frame_response(&DEMO_GET_VERSION);
                let last = payload.len() - 1;
                payload[last] ^= 0xff;
                new_message(
                    now - TimeDelta::seconds(30),
                    &alias,
                    MessageState::Incoming,
                    payload,
                )
            },
            new_message(
                now - TimeDelta::seconds(55),
                &http,
                MessageState::Incoming,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\xd0\x9f\xd1\x80\xd0\xb8\xd0\xb2\xd0\xb5\xd1\x82, hodeco!".to_vec(),
            ),
            new_message(
                now - TimeDelta::seconds(50),
                &http,
                MessageState::Incoming,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nこんにちは、hodeco。日本語のテスト。".as_bytes().to_vec(),
            ),
            new_message(
                now - TimeDelta::seconds(45),
                &http,
                MessageState::Incoming,
                utf16le_payload("Привет 日本 — UTF-16 LE"),
            ),
            new_message(
                now - TimeDelta::seconds(40),
                &http,
                MessageState::Incoming,
                utf16be_payload("Привет 日本 — UTF-16 BE"),
            ),
            new_message(
                now - TimeDelta::seconds(55),
                &http,
                MessageState::Outgoing,
                large_http_request_payload(100_000),
            ),
            new_message(
                now - TimeDelta::seconds(20),
                &http,
                MessageState::Incoming,
                large_http_response_payload(102_400),
            ),
            new_message(
                now - TimeDelta::seconds(18),
                &zigbee,
                MessageState::Incoming,
                vec![0x81, 0x60, 0x59, 0x7e],
            ),
            new_message(
                now - TimeDelta::seconds(16),
                &zigbee,
                MessageState::Outgoing,
                vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x04],
            ),
            new_message(
                now - TimeDelta::seconds(15),
                &zigbee,
                MessageState::Incoming,
                vec![0x00, 0x80, 0x01, 0x00, 0x00, 0x04, 0x02, 0x10, 0x54],
            ),
            new_message(
                now - TimeDelta::seconds(12),
                &zigbee,
                MessageState::Outgoing,
                vec![0x01, 0x00, 0x01, 0x18, 0x00],
            ),
            new_message(
                now - TimeDelta::seconds(11),
                &zigbee,
                MessageState::Incoming,
                vec![0xa1, 0x44, 0x3b, 0x7e],
            ),
            new_message(
                now - TimeDelta::seconds(10),
                &zigbee,
                MessageState::Incoming,
                vec![0x01, 0x80, 0x01, 0x18, 0x00, 0x02],
            ),
            new_message(
                now - TimeDelta::seconds(8),
                &zigbee,
                MessageState::Incoming,
                vec![
                    0x02, 0x80, 0x01, 0x45, 0x00, 0x00, 0x04, 0x01, 0x06, 0x00, 0x01, 0x01,
                    0x00, 0x00, 0x00, 0x00, 0x01, 0xff, 0xce, 0x34, 0x12, 0x00, 0x00, 0x07,
                    0x18, 0x05, 0x0a, 0x00, 0x00, 0x10, 0x01,
                ],
            ),
            new_message(
                now - TimeDelta::seconds(7),
                &zigbee,
                MessageState::Outgoing,
                zigbee::build_request(2, zigbee::EZSP_PERMIT_JOINING, &[180]),
            ),
            new_message(
                now - TimeDelta::seconds(7),
                &zigbee,
                MessageState::Incoming,
                zigbee_response(2, zigbee::EZSP_PERMIT_JOINING, &[0x00]),
            ),
            new_message(
                now - TimeDelta::seconds(6),
                &zigbee,
                MessageState::Outgoing,
                zigbee_send_unicast(4, 0x1234, 0x0006, &[0x01, 0x01, 0x01]),
            ),
            new_message(
                now - TimeDelta::seconds(6),
                &zigbee,
                MessageState::Incoming,
                zigbee_response(4, zigbee::EZSP_SEND_UNICAST, &[0x00, 0x01]),
            ),
            new_message(
                now - TimeDelta::seconds(5),
                &zigbee,
                MessageState::Outgoing,
                zigbee_send_unicast(
                    5,
                    0x1237,
                    0x0008,
                    &[0x01, 0x02, 0x00, 0x64, 0x0a, 0x00],
                ),
            ),
            new_message(
                now - TimeDelta::seconds(4),
                &zigbee,
                MessageState::Incoming,
                zigbee_incoming(
                    6,
                    0x1237,
                    0x0008,
                    &[0x18, 0x03, 0x0a, 0x00, 0x00, 0x20, 0x64],
                ),
            ),
            new_message(
                now - TimeDelta::seconds(3),
                &zigbee,
                MessageState::Incoming,
                zigbee_incoming(
                    7,
                    0x123a,
                    0x0402,
                    &[0x18, 0x04, 0x0a, 0x00, 0x00, 0x29, 0x66, 0x08],
                ),
            ),
            new_message(
                now - TimeDelta::seconds(2),
                &zigbee,
                MessageState::Outgoing,
                zigbee_send_unicast(
                    8,
                    0x1238,
                    0x0300,
                    &[0x01, 0x05, 0x00, 0x80, 0x00, 0x02, 0x00],
                ),
            ),
        ];
        messages.sort_by_key(|message| message.time);
        let mut last_time = messages.first().map(|message| message.time).unwrap_or(now);
        for message in messages.iter_mut().skip(1) {
            if message.time <= last_time {
                message.time = last_time
                    .checked_add_signed(TimeDelta::nanoseconds(1))
                    .unwrap_or(last_time);
            }
            last_time = message.time;
        }

        let config = Config::default();

        let commands = vec![
            CommandInfo {
                command: CommandDefinition {
                    id: "meter".to_string(),
                    name: "Meter poll".to_string(),
                    description: "Read the meter over the local bus".to_string(),
                    executable: "/usr/local/bin/meter-poll".into(),
                    args: vec!["--json".to_string()],
                    timeout: HumanDuration::from_std(std::time::Duration::from_secs(10)),
                    ..CommandDefinition::default()
                },
                used_by: vec!["evening-lights".to_string()],
            },
            CommandInfo {
                command: CommandDefinition {
                    id: "notify".to_string(),
                    name: "Notify desktop".to_string(),
                    description: "Send a desktop notification".to_string(),
                    executable: "notify-send".into(),
                    args: vec!["hodeco".to_string()],
                    timeout: HumanDuration::from_std(std::time::Duration::from_secs(5)),
                    ..CommandDefinition::default()
                },
                used_by: Vec::new(),
            },
        ];
        let schedule_timezone = Some("Europe/Berlin".to_string());
        let schedule_location = Some(Location::new(52.52, 13.405, Some(34.0)));
        let mut schedules = vec![
            ScheduleInfo {
                schedule: ScheduleDefinition {
                    id: "before-sunset".to_string(),
                    name: "Before sunset".to_string(),
                    description: "Fifteen minutes before sunset on weekdays".to_string(),
                    timezone: schedule_timezone.clone(),
                    location: Some(Location::new(52.52, 13.405, Some(34.0))),
                    at: OneOrMany::One(AtSpec::Text("sunset - 15m".to_string())),
                    repeat_at: OneOrMany::Many(vec![
                        HumanDuration::from_std(std::time::Duration::from_secs(30 * 60)),
                        HumanDuration::from_std(std::time::Duration::from_secs(60 * 60)),
                    ]),
                    ..ScheduleDefinition::default()
                },
                next: None,
                used_by: vec!["evening-lights".to_string()],
            },
            ScheduleInfo {
                schedule: ScheduleDefinition {
                    id: "morning".to_string(),
                    name: "Morning".to_string(),
                    at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                    days: vec![
                        hodeco_contract::domain::DaySpec::Weekday(chrono::Weekday::Mon),
                        hodeco_contract::domain::DaySpec::Weekday(chrono::Weekday::Tue),
                        hodeco_contract::domain::DaySpec::Weekday(chrono::Weekday::Wed),
                        hodeco_contract::domain::DaySpec::Weekday(chrono::Weekday::Thu),
                        hodeco_contract::domain::DaySpec::Weekday(chrono::Weekday::Fri),
                    ],
                    ..ScheduleDefinition::default()
                },
                next: None,
                used_by: Vec::new(),
            },
        ];
        let context = ScheduleContext {
            default_location: schedule_location,
            default_timezone: schedule_timezone
                .as_deref()
                .and_then(|timezone| timezone.parse().ok()),
        };
        for info in &mut schedules {
            info.next = hodeco_schedule::next_occurrence(&info.schedule, context, now, true)
                .ok()
                .flatten();
        }
        let evening = TaskDefinition {
            id: "evening-lights".to_string(),
            name: "Evening lights".to_string(),
            description: "Turn on the porch lights before sunset".to_string(),
            labels: demo_labels("area", 1, 24, &["lighting"]),
            triggers: vec![TaskTrigger::Schedule(ScheduleTrigger {
                id: "sunset".to_string(),
                schedule: "before-sunset".to_string(),
                max_runs: None,
            })],
            actions: vec![TaskAction {
                id: "meter".to_string(),
                kind: ActionKind::Command(CommandAction {
                    command: "meter".to_string(),
                    args: vec!["--once".to_string()],
                    output: OutputMode::Capture,
                }),
            }],
            ..TaskDefinition::default()
        };
        let motion = TaskDefinition {
            id: "porch-motion".to_string(),
            name: "Porch motion".to_string(),
            description: "Notify when the porch sensor reports motion".to_string(),
            labels: demo_labels("device", 1, 23, &["automated", "security"]),
            triggers: vec![TaskTrigger::Message(
                hodeco_contract::domain::MessageTrigger {
                    id: "motion".to_string(),
                    states: vec![MessageState::Incoming],
                    protocols: vec!["zwave".to_string()],
                    ..hodeco_contract::domain::MessageTrigger::default()
                },
            )],
            actions: vec![TaskAction {
                id: "notify".to_string(),
                kind: ActionKind::Message(MessageAction {
                    message: MessageActionPayload {
                        service: ServiceRef::Alias {
                            alias: "ZW".to_string(),
                        },
                        payload: Base64Bytes::from(b"Motion detected".to_vec()),
                        ttl: None,
                    },
                }),
            }],
            ..TaskDefinition::default()
        };
        let winter = TaskDefinition {
            id: "winter-lights".to_string(),
            name: "Winter lights".to_string(),
            description: "Seasonal lights; disabled outside winter".to_string(),
            labels: demo_labels("zone", 1, 24, &["seasonal"]),
            enabled: false,
            triggers: vec![TaskTrigger::Schedule(ScheduleTrigger {
                id: "winter-sunset".to_string(),
                schedule: "before-sunset".to_string(),
                max_runs: Some(120),
            })],
            ..TaskDefinition::default()
        };
        // The fourth task exists so the demo catalog can carry 100 distinct
        // labels without exceeding the 32-label limit on any one task.
        let label_catalog = TaskDefinition {
            id: "label-catalog".to_string(),
            name: "Label catalog".to_string(),
            description: "Demo task with a large label set for filter testing".to_string(),
            labels: demo_labels("automation", 1, 24, &["maintenance"]),
            ..TaskDefinition::default()
        };
        let now_summary = RunSummary {
            id: Uuid::new_v4(),
            task_id: evening.id.clone(),
            time: now - TimeDelta::minutes(30),
            started_at: now - TimeDelta::minutes(30),
            delayed_until: None,
            finished_at: now - TimeDelta::minutes(30) + TimeDelta::milliseconds(420),
            trigger: TriggerRef {
                id: "sunset".to_string(),
                kind: TriggerType::Schedule,
            },
            outcome: RunOutcome::Succeeded,
            action_count: 1,
            next_count: 0,
            error: None,
        };
        let failed_summary = RunSummary {
            id: Uuid::new_v4(),
            task_id: motion.id.clone(),
            time: now - TimeDelta::minutes(10),
            started_at: now - TimeDelta::minutes(10),
            delayed_until: None,
            finished_at: now - TimeDelta::minutes(10) + TimeDelta::milliseconds(80),
            trigger: TriggerRef {
                id: "motion".to_string(),
                kind: TriggerType::Message,
            },
            outcome: RunOutcome::Failed,
            action_count: 1,
            next_count: 0,
            error: Some("service is not ready".to_string()),
        };
        let runs = vec![
            TaskRun {
                summary: now_summary.clone(),
                trigger: RunTrigger::Schedule(hodeco_contract::domain::ScheduleRunTrigger {
                    id: "sunset".to_string(),
                    schedule_id: "before-sunset".to_string(),
                    occurrence_at: now - TimeDelta::minutes(30),
                }),
                actions: vec![RunAction {
                    id: "meter".to_string(),
                    started_at: now - TimeDelta::minutes(30),
                    finished_at: now - TimeDelta::minutes(30) + TimeDelta::milliseconds(400),
                    status: ActionOutcome::Succeeded,
                    command: Some(RunCommandAction {
                        command_id: "meter".to_string(),
                        executable: "meter-poll".to_string(),
                        args: vec!["--json".to_string(), "--once".to_string()],
                        exit_code: Some(0),
                        stdout: Some(Base64Bytes::from(b"{\"power\": 412}".to_vec())),
                        output_truncated: false,
                    }),
                    message: None,
                    error: None,
                }],
                selector: None,
                next_tasks: Vec::new(),
                chain: RunChain {
                    chain_id: Uuid::new_v4(),
                    parent_run_id: None,
                    depth: 0,
                },
                error: None,
            },
            TaskRun {
                summary: failed_summary,
                trigger: RunTrigger::Message(hodeco_contract::domain::MessageRunTrigger {
                    id: "motion".to_string(),
                    message_id: Uuid::new_v4(),
                    time: now - TimeDelta::minutes(10),
                    service: ServiceRef::Alias {
                        alias: "ZW".to_string(),
                    },
                    protocol: "zwave".to_string(),
                    transport: "serial".to_string(),
                    entry: "COM4".to_string(),
                    state: MessageState::Incoming,
                    payload: Some(Base64Bytes::from(vec![1, 2, 3])),
                }),
                actions: vec![RunAction {
                    id: "notify".to_string(),
                    started_at: now - TimeDelta::minutes(10),
                    finished_at: now - TimeDelta::minutes(10),
                    status: ActionOutcome::Failed,
                    command: None,
                    message: Some(RunMessageAction {
                        service: ServiceRef::Alias {
                            alias: "ZW".to_string(),
                        },
                        payload: Base64Bytes::from(vec![0x01, 0x03, 0x00]),
                        message_id: None,
                        ttl: None,
                    }),
                    error: Some("service is not ready".to_string()),
                }],
                selector: None,
                next_tasks: Vec::new(),
                chain: RunChain {
                    chain_id: Uuid::new_v4(),
                    parent_run_id: None,
                    depth: 0,
                },
                error: Some(hodeco_contract::domain::RunError {
                    stage: hodeco_contract::domain::RunStage::Actions,
                    message: "service is not ready".to_string(),
                }),
            },
        ];
        let running_run_id = Uuid::new_v4();
        let tasks = vec![
            TaskInfo {
                task: evening.clone(),
                running: false,
                running_run_id: None,
                last_run: Some(now_summary),
                next: Some(hodeco_contract::domain::NextFire {
                    at: now + TimeDelta::hours(2),
                    trigger_id: "sunset".to_string(),
                    trigger_type: TriggerType::Schedule,
                    schedule_id: Some("before-sunset".to_string()),
                }),
                suppressed_runs: 0,
                last_suppressed_reason: None,
            },
            TaskInfo {
                task: motion,
                running: true,
                running_run_id: Some(running_run_id),
                last_run: None,
                next: None,
                suppressed_runs: 2,
                last_suppressed_reason: Some("taskBusy".to_string()),
            },
            TaskInfo {
                task: winter,
                running: false,
                running_run_id: None,
                last_run: None,
                next: None,
                suppressed_runs: 0,
                last_suppressed_reason: None,
            },
            TaskInfo {
                task: label_catalog,
                running: false,
                running_run_id: None,
                last_run: None,
                next: None,
                suppressed_runs: 0,
                last_suppressed_reason: None,
            },
        ];

        Self {
            runtime_config: config.clone(),
            config,
            services,
            messages,
            last_time,
            subscribed: Vec::new(),
            discovery: None,
            discovery_finished: false,
            pending_outgoing: Vec::new(),
            started_at: now - TimeDelta::hours(2),
            ticks: 0,
            next_traffic: 0,
            tasks,
            schedules,
            schedule_location,
            schedule_timezone,
            commands,
            runs,
        }
    }

    /// Handles one request envelope and returns the response followed by any
    /// notifications caused by the request.
    pub fn handle(&mut self, text: &str) -> Vec<String> {
        let request = match serde_json::from_str::<Request>(text) {
            Ok(request) => request,
            Err(error) => return vec![invalid_request(text, &error.to_string())],
        };

        let id = request.id.clone();
        let mut notifications = Vec::new();
        let result = self.dispatch(&request, &mut notifications);

        let response = match result {
            Ok(value) => serde_json::to_string(&Response::success(id, value)),
            Err(error) => serde_json::to_string(&Response::error(id, error)),
        };
        let mut output = Vec::with_capacity(notifications.len() + 1);
        output.push(response.expect("demo response serializes"));
        output.extend(notifications);
        output
    }

    fn dispatch(
        &mut self,
        request: &Request,
        notifications: &mut Vec<String>,
    ) -> Result<Value, ApiError> {
        match request.method {
            Method::SystemRestart => {
                let _: Empty = parse(&request.params)?;
                Ok(json!({}))
            }
            Method::ServerInfo => to_value(ServerInfoResult {
                version: format!("{}-demo", env!("CARGO_PKG_VERSION")),
                protocol_version: DEMO_PROTOCOL_VERSION.to_string(),
                started_at: self.started_at,
            }),
            Method::ConfigGet => to_value(self.config()),
            Method::ConfigUpdate => {
                let params: ConfigUpdateParams = parse(&request.params)?;
                let config = params.config;
                validate_demo_config(&config)?;
                let restart_required = demo_restart_required(&self.runtime_config, &config);
                self.config = config.clone();
                to_value(ConfigUpdateResult {
                    config,
                    restart_required,
                })
            }
            Method::ProtocolsList => to_value(ProtocolsListResult {
                protocols: protocol_infos(),
            }),
            Method::TransportsList => to_value(TransportsListResult {
                transports: transport_infos(),
            }),
            Method::ProtocolsInfo => {
                let params: ProtocolInfoParams = parse(&request.params)?;
                let protocol = match params.protocol.as_str() {
                    "zwave" => catalog::zwave_protocol_info(),
                    "zigbee" => catalog::zigbee_protocol_info(),
                    "http" => http_protocol_info(),
                    _ => {
                        return Err(ApiError::new(
                            ErrorCode::UnknownProtocol,
                            format!("unknown protocol '{}'", params.protocol),
                        ));
                    }
                };
                to_value(ProtocolInfoResult { protocol })
            }
            Method::ProtocolsParams => {
                let params: ProtocolParamsParams = parse(&request.params)?;
                let params = protocol_transport_params(&params.protocol, &params.transport)?;
                to_value(ProtocolParamsResult { params })
            }
            Method::DiscoveryStart => {
                let params: DiscoveryStartParams = parse(&request.params)?;
                if !matches!(params.protocol.as_str(), "zwave" | "zigbee" | "http") {
                    return Err(ApiError::new(
                        ErrorCode::UnknownProtocol,
                        format!("unknown protocol '{}'", params.protocol),
                    ));
                }
                if !matches!(params.transport.as_str(), "serial" | "tcp") {
                    return Err(ApiError::new(
                        ErrorCode::UnknownTransport,
                        format!("unknown transport '{}'", params.transport),
                    ));
                }
                let discovery = DiscoveryInfo {
                    id: Uuid::new_v4(),
                    protocol: params.protocol,
                    transport: params.transport,
                    state: DiscoveryState::Running,
                    entries: Vec::new(),
                };
                self.discovery = Some(discovery.clone());
                self.discovery_finished = false;
                notifications.push(notification(Notification::DiscoveryStarted(
                    DiscoveryNotification {
                        discovery: discovery.clone(),
                    },
                )));
                to_value(DiscoveryStartResult {
                    discovery_id: discovery.id,
                })
            }
            Method::DiscoveryGet => {
                let params: DiscoveryGetParams = parse(&request.params)?;
                match &self.discovery {
                    Some(discovery) if params.discovery_id.is_none_or(|id| id == discovery.id) => {
                        to_value(DiscoveryGetResult {
                            discovery: discovery.clone(),
                        })
                    }
                    _ => Err(ApiError::new(
                        ErrorCode::DiscoveryNotFound,
                        "no discovery has been started",
                    )),
                }
            }
            Method::DiscoveryCancel => {
                let params: DiscoveryCancelParams = parse(&request.params)?;
                match &mut self.discovery {
                    Some(discovery) if discovery.id == params.discovery_id => {
                        discovery.state = DiscoveryState::Cancelled;
                        self.discovery_finished = true;
                        let finished = discovery.clone();
                        notifications.push(notification(Notification::DiscoveryFinished(
                            DiscoveryNotification {
                                discovery: finished,
                            },
                        )));
                        Ok(json!({}))
                    }
                    _ => Err(ApiError::new(
                        ErrorCode::DiscoveryNotFound,
                        "discovery not found",
                    )),
                }
            }
            Method::ServicesList => to_value(ServicesListResult {
                services: self.services.clone(),
            }),
            Method::ServicesAdd => {
                let params: ServicesAddParams = parse(&request.params)?;
                let key = ServiceRef::Key {
                    protocol: params.protocol.clone(),
                    transport: params.transport.clone(),
                    entry: params.entry.clone(),
                };
                if self.services.iter().any(|service| same_key(service, &key)) {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAlreadyExists,
                        format!("a service for {} already exists", params.entry),
                    ));
                }
                let alias = params
                    .alias
                    .as_deref()
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(str::to_string);
                if let Some(alias) = &alias
                    && self
                        .services
                        .iter()
                        .any(|service| service.alias.as_deref() == Some(alias.as_str()))
                {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAliasConflict,
                        format!("the alias '{alias}' is already in use"),
                    ));
                }
                let detail =
                    (params.protocol == "zigbee").then(|| "EZSP v4 · stack 0x5410".to_string());
                let service = ServiceInfo {
                    alias,
                    protocol: params.protocol,
                    transport: params.transport,
                    entry: params.entry,
                    status: ServiceStatus {
                        state: ServiceState::Running,
                        error: None,
                        detail,
                    },
                    params: params.params,
                };
                self.services.push(service.clone());
                to_value(ServicesAddResult { service })
            }
            Method::ServicesRemove => {
                let params: ServicesRemoveParams = parse(&request.params)?;
                let Some(index) = self
                    .services
                    .iter()
                    .position(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                self.services.remove(index);
                Ok(json!({}))
            }
            Method::ServicesAlias => {
                let params: ServicesAliasParams = parse(&request.params)?;
                let requested = params
                    .alias
                    .as_deref()
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(str::to_string);
                if let Some(alias) = &requested
                    && self.services.iter().any(|service| {
                        service.alias.as_deref() == Some(alias.as_str())
                            && !matches_ref(service, &params.service)
                    })
                {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAliasConflict,
                        format!("the alias '{alias}' is already in use"),
                    ));
                }
                let Some(service) = self
                    .services
                    .iter_mut()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                service.alias = requested;
                to_value(ServicesAliasResult {
                    service: service.clone(),
                })
            }
            Method::ServicesStatus => {
                let params: ServicesStatusParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                to_value(ServicesStatusResult {
                    status: service.status.clone(),
                })
            }
            Method::ServicesParams => {
                let params: ServicesParamsParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter_mut()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                service.params = params.params.clone();
                to_value(ServicesParamsResult {
                    service: service.clone(),
                })
            }
            Method::ServicesSend => {
                let params: ServicesSendParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                if service.status.state != ServiceState::Running {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotReady,
                        "service is not running",
                    ));
                }
                let time = self.next_time(Utc::now());
                let message = new_message(
                    time,
                    &params.service,
                    MessageState::OutgoingPending,
                    params.payload.into_inner(),
                );
                self.pending_outgoing.push(message.id);
                self.messages.push(message.clone());
                notifications.push(notification(Notification::MessageNew(
                    MessageNotification {
                        message: message.clone(),
                    },
                )));
                to_value(ServicesSendResult { message })
            }
            Method::MessagesGet => {
                let params: MessagesGetParams = parse(&request.params)?;
                let Some(message) = self.messages.iter().find(|message| message.id == params.id)
                else {
                    return Err(ApiError::new(
                        ErrorCode::MessageNotFound,
                        "message not found",
                    ));
                };
                to_value(MessagesGetResult {
                    message: message.clone(),
                })
            }
            Method::MessagesList => {
                let params: MessagesListParams = parse(&request.params)?;
                let limit = params.limit.unwrap_or(100).clamp(1, 10_000) as usize;
                if params
                    .from
                    .is_some_and(|from| params.until.is_some_and(|until| from >= until))
                {
                    return to_value(MessagesListResult {
                        messages: Vec::new(),
                        next: None,
                        store_total: self.messages.len() as u64,
                    });
                }
                let ids: HashSet<Uuid> = params.ids.iter().copied().collect();
                let mut selected: Vec<Message> = self
                    .messages
                    .iter()
                    .filter(|message| {
                        params.from.is_none_or(|from| message.time >= from)
                            && params.until.is_none_or(|until| message.time < until)
                            && (ids.is_empty() || ids.contains(&message.id))
                            && self.message_matches(message, &params)
                    })
                    .cloned()
                    .collect();
                selected.sort_by_key(|message| message.time);
                if params.order == MessageOrder::NewestFirst {
                    selected.reverse();
                }
                let has_more = selected.len() > limit;
                selected.truncate(limit);
                let next = if has_more {
                    selected.last().map(|message| match params.order {
                        MessageOrder::NewestFirst => MessageTimeRange {
                            from: params.from,
                            until: Some(message.time),
                        },
                        MessageOrder::OldestFirst => MessageTimeRange {
                            from: message
                                .time
                                .checked_add_signed(TimeDelta::nanoseconds(1))
                                .or(params.from),
                            until: params.until,
                        },
                    })
                } else {
                    None
                };
                to_value(MessagesListResult {
                    messages: selected,
                    next,
                    store_total: self.messages.len() as u64,
                })
            }
            Method::TasksList => to_value(TasksListResult {
                tasks: self.tasks.clone(),
            }),
            Method::TasksGet => {
                let params: TasksGetParams = parse(&request.params)?;
                let Some(task) = self
                    .tasks
                    .iter()
                    .find(|info| info.task.id == params.task_id)
                    .cloned()
                else {
                    return Err(ApiError::new(ErrorCode::TaskNotFound, "task not found"));
                };
                to_value(TasksGetResult { task })
            }
            Method::TasksGetRun => {
                let params: TasksGetRunParams = parse(&request.params)?;
                let Some(run) = self
                    .runs
                    .iter()
                    .find(|run| {
                        run.summary.task_id == params.task_id && run.summary.id == params.run_id
                    })
                    .cloned()
                else {
                    return Err(ApiError::new(ErrorCode::RunNotFound, "run not found"));
                };
                to_value(TasksGetRunResult { run })
            }
            Method::TasksAdd => {
                let params: TasksAddParams = parse(&request.params)?;
                let mut task = params.task;
                if task.id.is_empty() {
                    task.id = id_from_name("task", &task.name);
                }
                if self.tasks.iter().any(|info| info.task.id == task.id) {
                    return Err(ApiError::new(
                        ErrorCode::InvalidParams,
                        "task already exists",
                    ));
                }
                let info = TaskInfo {
                    task,
                    running: false,
                    running_run_id: None,
                    last_run: None,
                    next: None,
                    suppressed_runs: 0,
                    last_suppressed_reason: None,
                };
                self.tasks.push(info.clone());
                to_value(TasksAddResult { task: info })
            }
            Method::TasksUpdate => {
                let params: TasksUpdateParams = parse(&request.params)?;
                let Some(info) = self
                    .tasks
                    .iter_mut()
                    .find(|info| info.task.id == params.task_id)
                else {
                    return Err(ApiError::new(ErrorCode::TaskNotFound, "task not found"));
                };
                let mut task = params.task;
                task.id = params.task_id;
                info.task = task;
                to_value(TasksUpdateResult { task: info.clone() })
            }
            Method::TasksRemove => {
                let params: TasksRemoveParams = parse(&request.params)?;
                self.tasks.retain(|info| info.task.id != params.task_id);
                self.runs
                    .retain(|run| run.summary.task_id != params.task_id);
                Ok(json!({}))
            }
            Method::TasksFire => {
                let params: TasksFireParams = parse(&request.params)?;
                let Some(index) = self
                    .tasks
                    .iter()
                    .position(|info| info.task.id == params.task_id)
                else {
                    return Err(ApiError::new(ErrorCode::TaskNotFound, "task not found"));
                };
                if !self.tasks[index].task.enabled {
                    return Err(ApiError::new(ErrorCode::TaskDisabled, "task is disabled"));
                }
                let task_id = self.tasks[index].task.id.clone();
                let run_id = Uuid::new_v4();
                let now = self.next_time(Utc::now());
                let summary = RunSummary {
                    id: run_id,
                    task_id: task_id.clone(),
                    time: now,
                    started_at: now,
                    delayed_until: None,
                    finished_at: now + TimeDelta::milliseconds(30),
                    trigger: TriggerRef {
                        id: "manual".to_string(),
                        kind: TriggerType::Manual,
                    },
                    outcome: RunOutcome::Succeeded,
                    action_count: 0,
                    next_count: 0,
                    error: None,
                };
                self.runs.push(TaskRun {
                    summary: summary.clone(),
                    trigger: RunTrigger::Manual(ManualRunTrigger {
                        id: "manual".to_string(),
                        payload: params.payload,
                        metadata: params.metadata,
                    }),
                    actions: Vec::new(),
                    selector: None,
                    next_tasks: Vec::new(),
                    chain: RunChain {
                        chain_id: Uuid::new_v4(),
                        parent_run_id: None,
                        depth: 0,
                    },
                    error: None,
                });
                self.tasks[index].last_run = Some(summary);
                to_value(TasksFireResult { run_id })
            }
            Method::TasksCancelRun => {
                let params: TasksCancelRunParams = parse(&request.params)?;
                let Some(index) = self
                    .tasks
                    .iter()
                    .position(|info| info.task.id == params.task_id)
                else {
                    return Err(ApiError::new(ErrorCode::TaskNotFound, "task not found"));
                };
                if !self.tasks[index].running
                    || self.tasks[index].running_run_id != Some(params.run_id)
                {
                    return Err(ApiError::new(ErrorCode::RunNotFound, "run is not active"));
                }
                let now = self.next_time(Utc::now());
                let summary = RunSummary {
                    id: params.run_id,
                    task_id: params.task_id.clone(),
                    time: now,
                    started_at: now - TimeDelta::seconds(1),
                    delayed_until: None,
                    finished_at: now,
                    trigger: TriggerRef {
                        id: "manual".to_string(),
                        kind: TriggerType::Manual,
                    },
                    outcome: RunOutcome::Cancelled,
                    action_count: 0,
                    next_count: 0,
                    error: Some("run cancelled by user".to_string()),
                };
                // The demo has no real execution to archive. Cancelling clears
                // the active run instead of manufacturing a history row.
                self.runs.retain(|run| run.summary.id != params.run_id);
                self.tasks[index].running = false;
                self.tasks[index].running_run_id = None;
                notifications.push(notification(Notification::TaskRunFinished(
                    TaskRunFinishedNotification { run: summary },
                )));
                Ok(json!({}))
            }
            Method::TasksHistory => {
                let params: TasksHistoryParams = parse(&request.params)?;
                let mut runs: Vec<RunSummary> = self
                    .runs
                    .iter()
                    .map(|run| run.summary.clone())
                    .filter(|run| {
                        (params.task_ids.is_empty() || params.task_ids.contains(&run.task_id))
                            && (params.trigger_ids.is_empty()
                                || params.trigger_ids.contains(&run.trigger.id))
                            && (params.types.is_empty() || params.types.contains(&run.trigger.kind))
                            && (params.outcomes.is_empty()
                                || params.outcomes.contains(&run.outcome))
                    })
                    .collect();
                let (runs, next) = demo_run_page(
                    &mut runs,
                    params.from,
                    params.until,
                    params.order,
                    params.limit,
                );
                let store_totals = self
                    .tasks
                    .iter()
                    .filter(|info| {
                        params.task_ids.is_empty() || params.task_ids.contains(&info.task.id)
                    })
                    .map(|info| TaskStoreTotal {
                        task_id: info.task.id.clone(),
                        rows: self
                            .runs
                            .iter()
                            .filter(|run| run.summary.task_id == info.task.id)
                            .count() as u64,
                        bytes: 0,
                    })
                    .collect();
                to_value(TasksHistoryResult {
                    runs,
                    next,
                    store_totals,
                })
            }
            Method::TasksRunFeed => {
                let params: TasksRunFeedParams = parse(&request.params)?;
                let max_rows = self.config.task_engine.run_feed.max_rows.max(1) as usize;
                let skip = self.runs.len().saturating_sub(max_rows);
                let mut runs: Vec<RunSummary> = self
                    .runs
                    .iter()
                    .skip(skip)
                    .map(|run| run.summary.clone())
                    .filter(|run| {
                        (params.task_ids.is_empty() || params.task_ids.contains(&run.task_id))
                            && (params.trigger_ids.is_empty()
                                || params.trigger_ids.contains(&run.trigger.id))
                            && (params.types.is_empty() || params.types.contains(&run.trigger.kind))
                            && (params.outcomes.is_empty()
                                || params.outcomes.contains(&run.outcome))
                    })
                    .collect();
                let (runs, next) = demo_run_page(
                    &mut runs,
                    params.from,
                    params.until,
                    params.order,
                    params.limit,
                );
                to_value(TasksRunFeedResult {
                    runs,
                    next,
                    feed: RunFeedTotals {
                        rows: self.runs.len().min(max_rows) as u64,
                        bytes: 0,
                        max_rows: max_rows as u32,
                        max_size: self.config.task_engine.run_feed.max_size.as_bytes(),
                    },
                })
            }
            Method::TasksPurgeHistory => {
                let params: TasksPurgeHistoryParams = parse(&request.params)?;
                self.runs
                    .retain(|run| run.summary.task_id != params.task_id);
                if let Some(info) = self
                    .tasks
                    .iter_mut()
                    .find(|info| info.task.id == params.task_id)
                {
                    info.last_run = None;
                }
                Ok(json!({}))
            }
            Method::SchedulesList => to_value(SchedulesListResult {
                schedules: self.schedules.clone(),
                location: self.schedule_location,
                timezone: self.schedule_timezone.clone(),
                system_timezone: Some("UTC".to_string()),
            }),
            Method::SchedulesGet => {
                let params: SchedulesGetParams = parse(&request.params)?;
                let Some(schedule) = self
                    .schedules
                    .iter()
                    .find(|info| info.schedule.id == params.schedule_id)
                    .cloned()
                else {
                    return Err(ApiError::new(
                        ErrorCode::ScheduleNotFound,
                        "schedule not found",
                    ));
                };
                to_value(SchedulesGetResult { schedule })
            }
            Method::SchedulesAdd => {
                let params: SchedulesAddParams = parse(&request.params)?;
                let mut schedule = params.schedule;
                if schedule.id.is_empty() {
                    schedule.id = id_from_name("schedule", &schedule.name);
                }
                let info = ScheduleInfo {
                    schedule,
                    next: None,
                    used_by: Vec::new(),
                };
                self.schedules.push(info.clone());
                to_value(SchedulesAddResult { schedule: info })
            }
            Method::SchedulesUpdate => {
                let params: SchedulesUpdateParams = parse(&request.params)?;
                let Some(info) = self
                    .schedules
                    .iter_mut()
                    .find(|info| info.schedule.id == params.schedule_id)
                else {
                    return Err(ApiError::new(
                        ErrorCode::ScheduleNotFound,
                        "schedule not found",
                    ));
                };
                let mut schedule = params.schedule;
                schedule.id = params.schedule_id;
                info.schedule = schedule;
                to_value(SchedulesUpdateResult {
                    schedule: info.clone(),
                })
            }
            Method::SchedulesRemove => {
                let params: SchedulesRemoveParams = parse(&request.params)?;
                let used = self
                    .tasks
                    .iter()
                    .any(|info| task_uses_schedule(&info.task, &params.schedule_id));
                if used {
                    return Err(ApiError::new(
                        ErrorCode::ScheduleInUse,
                        "schedule is used by tasks",
                    ));
                }
                self.schedules
                    .retain(|info| info.schedule.id != params.schedule_id);
                Ok(json!({}))
            }
            Method::SchedulesPreview => {
                let params: SchedulesPreviewParams = parse(&request.params)?;
                let limit = params.limit.unwrap_or(20).clamp(1, 100) as usize;
                let now = self.next_time(Utc::now());
                let context = ScheduleContext {
                    default_location: self.schedule_location,
                    default_timezone: self
                        .schedule_timezone
                        .as_deref()
                        .and_then(|timezone| timezone.parse().ok()),
                };
                let occurrences =
                    hodeco_schedule::preview(&params.schedule, context, now, limit, true)
                        .map_err(|error| ApiError::new(ErrorCode::ConfigInvalid, error))?;
                to_value(SchedulesPreviewResult { occurrences })
            }
            Method::SchedulesSetDefaults => {
                let params: SchedulesSetDefaultsParams = parse(&request.params)?;
                self.schedule_location = params.location;
                self.schedule_timezone = params.timezone;
                to_value(SchedulesSetDefaultsResult {
                    location: self.schedule_location,
                    timezone: self.schedule_timezone.clone(),
                })
            }
            Method::SchedulesSolarPreview => {
                let params: SchedulesSolarPreviewParams = parse(&request.params)?;
                let events = SolarEvent::ALL
                    .into_iter()
                    .map(|event| {
                        let local = match event {
                            SolarEvent::Sunrise => "06:15:00",
                            SolarEvent::Sunset => "18:45:00",
                            SolarEvent::Dawn => "05:30:00",
                            SolarEvent::Dusk => "19:30:00",
                            SolarEvent::Noon => "12:30:00",
                            SolarEvent::Nadir => "00:30:00",
                        };
                        SolarPreviewEvent {
                            event,
                            at: None,
                            local: Some(local.to_string()),
                            reason: None,
                        }
                    })
                    .collect();
                to_value(SchedulesSolarPreviewResult {
                    date: params.date,
                    timezone: params
                        .timezone
                        .or_else(|| self.schedule_timezone.clone())
                        .unwrap_or_else(|| "UTC".to_string()),
                    events,
                })
            }
            Method::CommandsList => to_value(CommandsListResult {
                commands: self.commands.clone(),
            }),
            Method::CommandsGet => {
                let params: CommandsGetParams = parse(&request.params)?;
                let Some(command) = self
                    .commands
                    .iter()
                    .find(|info| info.command.id == params.command_id)
                    .cloned()
                else {
                    return Err(ApiError::new(
                        ErrorCode::CommandNotFound,
                        "command not found",
                    ));
                };
                to_value(CommandsGetResult { command })
            }
            Method::CommandsAdd => {
                let params: CommandsAddParams = parse(&request.params)?;
                let mut command = params.command;
                if command.id.is_empty() {
                    command.id = id_from_name("command", &command.name);
                }
                let info = CommandInfo {
                    command,
                    used_by: Vec::new(),
                };
                self.commands.push(info.clone());
                to_value(CommandsAddResult { command: info })
            }
            Method::CommandsUpdate => {
                let params: CommandsUpdateParams = parse(&request.params)?;
                let Some(info) = self
                    .commands
                    .iter_mut()
                    .find(|info| info.command.id == params.command_id)
                else {
                    return Err(ApiError::new(
                        ErrorCode::CommandNotFound,
                        "command not found",
                    ));
                };
                let mut command = params.command;
                command.id = params.command_id;
                info.command = command;
                to_value(CommandsUpdateResult {
                    command: info.clone(),
                })
            }
            Method::CommandsRemove => {
                let params: CommandsRemoveParams = parse(&request.params)?;
                let used = self
                    .tasks
                    .iter()
                    .any(|info| task_uses_command(&info.task, &params.command_id));
                if used {
                    return Err(ApiError::new(
                        ErrorCode::CommandInUse,
                        "command is used by tasks",
                    ));
                }
                self.commands
                    .retain(|info| info.command.id != params.command_id);
                Ok(json!({}))
            }
            Method::EventsSubscribe => {
                let params: EventsSubscribeParams = parse(&request.params)?;
                for topic in params.topics {
                    if !self.subscribed.contains(&topic) {
                        self.subscribed.push(topic);
                    }
                }
                Ok(json!({}))
            }
            Method::EventsUnsubscribe => {
                let params: EventsUnsubscribeParams = parse(&request.params)?;
                for topic in params.topics {
                    if let Some(position) = self.subscribed.iter().position(|value| *value == topic)
                    {
                        self.subscribed.remove(position);
                    }
                }
                Ok(json!({}))
            }
        }
    }

    /// Produces live traffic and discovery transitions. Called on a timer by
    /// the demo transport.
    pub fn tick(&mut self) -> Vec<String> {
        self.ticks += 1;
        let mut output = Vec::new();

        if let Some(discovery) = &mut self.discovery
            && discovery.state == DiscoveryState::Running
            && !self.discovery_finished
        {
            self.discovery_finished = true;
            discovery.state = DiscoveryState::Finished;
            discovery.entries = discovery_entries(&discovery.protocol);
            let finished = discovery.clone();
            output.push(notification(Notification::DiscoveryFinished(
                DiscoveryNotification {
                    discovery: finished,
                },
            )));
        }

        // Promote outgoing messages one tick after they were queued.
        let pending = std::mem::take(&mut self.pending_outgoing);
        for id in pending {
            if let Some(message) = self.messages.iter_mut().find(|message| message.id == id)
                && message.state == MessageState::OutgoingPending
            {
                message.state = MessageState::Outgoing;
                output.push(notification(Notification::MessageStateChanged(
                    MessageNotification {
                        message: message.clone(),
                    },
                )));
            }
        }

        if self.subscribed.contains(&EventTopic::Messages) {
            let payload = traffic_payload(self.next_traffic);
            self.next_traffic += 1;
            let time = self.next_time(Utc::now());
            let message = new_message(
                time,
                &self.traffic_service(),
                MessageState::Incoming,
                payload,
            );
            self.messages.push(message.clone());
            output.push(notification(Notification::MessageNew(
                MessageNotification { message },
            )));
        }

        output
    }

    fn message_matches(&self, message: &Message, params: &MessagesListParams) -> bool {
        if !params.states.is_empty() && !params.states.contains(&message.state) {
            return false;
        }
        if !params.directions.is_empty() && !params.directions.contains(&message.state.direction())
        {
            return false;
        }
        let (protocol, transport) = self.message_service_key(message);
        if !params.protocols.is_empty() && !params.protocols.contains(&protocol) {
            return false;
        }
        if !params.transports.is_empty() && !params.transports.contains(&transport) {
            return false;
        }
        if !params.services.is_empty()
            && !params
                .services
                .iter()
                .any(|reference| matches_message_ref(message, reference))
        {
            return false;
        }
        if let Some(payload) = &params.payload
            && !payload.matches(message.payload.as_slice())
        {
            return false;
        }
        true
    }

    fn message_service_key(&self, message: &Message) -> (String, String) {
        match &message.service {
            ServiceRef::Key {
                protocol,
                transport,
                ..
            } => (protocol.clone(), transport.clone()),
            ServiceRef::Alias { alias } => self
                .services
                .iter()
                .find(|service| service.alias.as_deref() == Some(alias.as_str()))
                .map(|service| (service.protocol.clone(), service.transport.clone()))
                .unwrap_or_default(),
        }
    }

    fn next_time(&mut self, now: DateTime<Utc>) -> DateTime<Utc> {
        let time = if now <= self.last_time {
            self.last_time
                .checked_add_signed(TimeDelta::nanoseconds(1))
                .unwrap_or(now)
        } else {
            now
        };
        self.last_time = time;
        time
    }

    fn traffic_service(&self) -> ServiceRef {
        self.services
            .iter()
            .find(|service| service.status.state == ServiceState::Running)
            .map(|service| match &service.alias {
                Some(alias) => ServiceRef::Alias {
                    alias: alias.clone(),
                },
                None => ServiceRef::Key {
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                },
            })
            .unwrap_or_else(|| ServiceRef::Alias {
                alias: "ZW".to_string(),
            })
    }

    pub fn config(&self) -> Config {
        self.config.clone()
    }
}

fn invalid_request(text: &str, reason: &str) -> String {
    let value = serde_json::from_str::<Value>(text).ok();
    let id = value
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(method) = value
        .as_ref()
        .and_then(|value| value.get("method"))
        .and_then(Value::as_str)
        && method.parse::<Method>().is_err()
    {
        return response_error(
            id,
            ApiError::new(
                ErrorCode::UnknownMethod,
                format!("unknown method '{method}'"),
            ),
        );
    }
    response_error(
        id,
        ApiError::new(
            ErrorCode::InvalidRequest,
            format!("invalid request: {reason}"),
        ),
    )
}

fn response_error(id: &str, error: ApiError) -> String {
    serde_json::to_string(&Response::error(id, error)).expect("error response serializes")
}

fn parse<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, ApiError> {
    serde_json::from_value(params.clone()).map_err(|error| {
        ApiError::new(ErrorCode::InvalidParams, format!("invalid params: {error}"))
    })
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value)
        .map_err(|error| ApiError::new(ErrorCode::InternalError, error.to_string()))
}

fn demo_run_page(
    runs: &mut Vec<RunSummary>,
    from: Option<chrono::DateTime<Utc>>,
    until: Option<chrono::DateTime<Utc>>,
    order: MessageOrder,
    limit: Option<u32>,
) -> (Vec<RunSummary>, Option<MessageTimeRange>) {
    if from.is_some_and(|from| until.is_some_and(|until| from >= until)) {
        return (Vec::new(), None);
    }
    runs.retain(|run| {
        from.is_none_or(|from| run.time >= from) && until.is_none_or(|until| run.time < until)
    });
    runs.sort_by_key(|run| run.time);
    let limit = limit.unwrap_or(100).clamp(1, 10_000) as usize;
    let has_more = runs.len() > limit;
    let page: Vec<RunSummary> = match order {
        MessageOrder::NewestFirst => runs.iter().rev().take(limit).cloned().collect(),
        MessageOrder::OldestFirst => runs.iter().take(limit).cloned().collect(),
    };
    let next = if has_more {
        page.last().map(|run| match order {
            MessageOrder::NewestFirst => MessageTimeRange {
                from,
                until: Some(run.time),
            },
            MessageOrder::OldestFirst => MessageTimeRange {
                from: run
                    .time
                    .checked_add_signed(chrono::TimeDelta::nanoseconds(1))
                    .or(from),
                until,
            },
        })
    } else {
        None
    };
    (page, next)
}

fn notification(notification: Notification) -> String {
    serde_json::to_string(&notification).expect("notification serializes")
}

fn new_message(
    time: DateTime<Utc>,
    service: &ServiceRef,
    state: MessageState,
    payload: Vec<u8>,
) -> Message {
    Message {
        id: Uuid::new_v4(),
        time,
        service: service.clone(),
        state,
        payload: Base64Bytes::from(payload),
    }
}

fn zigbee_response(seq: u8, command: u16, params: &[u8]) -> Vec<u8> {
    let mut frame = zigbee::build_request(seq, command, params);
    frame[1] |= 0x80;
    frame
}

fn zigbee_send_unicast(seq: u8, destination: u16, cluster: u16, zcl: &[u8]) -> Vec<u8> {
    let mut params = vec![
        0x00, // direct
        destination as u8,
        (destination >> 8) as u8,
        0x04,
        0x01, // Home Automation profile
        cluster as u8,
        (cluster >> 8) as u8,
        0x01, // source endpoint
        0x01, // destination endpoint
        0x00,
        0x00, // APS options
        0x00,
        0x00, // group ID
        0x01, // APS sequence
        0x01, // message tag
        zcl.len() as u8,
    ];
    params.extend_from_slice(zcl);
    zigbee::build_request(seq, zigbee::EZSP_SEND_UNICAST, &params)
}

fn zigbee_incoming(seq: u8, sender: u16, cluster: u16, zcl: &[u8]) -> Vec<u8> {
    let mut params = vec![
        0x00, // unicast
        0x04,
        0x01, // Home Automation profile
        cluster as u8,
        (cluster >> 8) as u8,
        0x01, // source endpoint
        0x01, // destination endpoint
        0x00,
        0x00, // APS options
        0x00,
        0x00, // group ID
        0x01, // APS sequence
        0xff, // LQI
        0xce, // RSSI
        sender as u8,
        (sender >> 8) as u8,
        0x00, // binding index
        0x00, // address index
        zcl.len() as u8,
    ];
    params.extend_from_slice(zcl);
    zigbee_response(seq, zigbee::EZSP_INCOMING_MESSAGE_HANDLER, &params)
}

fn traffic_payload(index: usize) -> Vec<u8> {
    match index % 5 {
        0 => frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x45]),
        1 => frame_request(&[0x04, 0x00, 0x02, 0x03, 0x25, 0x03, 0xff]),
        2 => frame_request(&[0x04, 0x00, 0x07, 0x04, 0x26, 0x03, 0x48, 0x05]),
        3 => frame_request(&[
            0x04, 0x00, 0x06, 0x08, 0x32, 0x02, 0x01, 0x00, 0x00, 0x00, 0x30, 0x39,
        ]),
        _ => frame_response(&DEMO_GET_VERSION),
    }
}

fn utf16le_payload(text: &str) -> Vec<u8> {
    let mut payload = vec![0xff, 0xfe];
    payload.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    payload
}

fn utf16be_payload(text: &str) -> Vec<u8> {
    let mut payload = vec![0xfe, 0xff];
    payload.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    payload
}

fn large_http_request_payload(size: usize) -> Vec<u8> {
    large_http_payload(
        size,
        "POST /api/v1/telemetry HTTP/1.1\r\nHost: api.example.test\r\nContent-Type: application/json\r\nX-Demo-Message: large-http-request\r\n\r\n",
    )
}

fn large_http_response_payload(size: usize) -> Vec<u8> {
    large_http_payload(
        size,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Demo-Message: large-http-response\r\n\r\n",
    )
}

/// Builds a JSON-shaped HTTP body of exactly `size` bytes so the demo can
/// exercise large raw payloads without any protocol-specific parser.
fn large_http_payload(size: usize, header: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(size);
    payload.extend_from_slice(header.as_bytes());
    payload.extend_from_slice(b"{\"demo\":\"large-http\",\"padding\":\"");
    let suffix = b"\"}";
    let body_len = size.saturating_sub(payload.len() + suffix.len());
    let pattern = b"0123456789abcdef";
    payload.extend(pattern.iter().copied().cycle().take(body_len));
    payload.extend_from_slice(suffix);
    payload.truncate(size);
    payload
}

fn discovery_entries(protocol: &str) -> Vec<DiscoveryEntry> {
    if protocol == "zigbee" {
        return vec![
            DiscoveryEntry {
                entry: "COM4".to_string(),
                available: false,
                description: None,
                error: Some("no EZSP version response".to_string()),
            },
            DiscoveryEntry {
                entry: "COM5".to_string(),
                available: false,
                description: None,
                error: Some("port is already in use by ZB".to_string()),
            },
            DiscoveryEntry {
                entry: "COM8".to_string(),
                available: true,
                description: Some(
                    "Nortec HUSBZB-1 ZigBee Hub [EZSP v4 · stack 0x5410]".to_string(),
                ),
                error: None,
            },
        ];
    }
    if protocol == "http" {
        vec![
            DiscoveryEntry {
                entry: "api.example.test:443".to_string(),
                available: true,
                description: Some("Demo HTTP endpoint".to_string()),
                error: None,
            },
            DiscoveryEntry {
                entry: "localhost:8080".to_string(),
                available: true,
                description: Some("Local HTTP endpoint".to_string()),
                error: None,
            },
            DiscoveryEntry {
                entry: "blocked.example.test:80".to_string(),
                available: false,
                description: None,
                error: Some("connection refused".to_string()),
            },
        ]
    } else {
        vec![
            DiscoveryEntry {
                entry: "COM3".to_string(),
                available: false,
                description: None,
                error: Some("access denied".to_string()),
            },
            DiscoveryEntry {
                entry: "COM4".to_string(),
                available: false,
                description: None,
                error: Some("port is already in use by ZW".to_string()),
            },
            DiscoveryEntry {
                entry: "COM7".to_string(),
                available: false,
                description: None,
                error: Some("port is already in use by ZW-Lab".to_string()),
            },
            DiscoveryEntry {
                entry: "COM6".to_string(),
                available: true,
                description: Some("USB serial controller".to_string()),
                error: None,
            },
        ]
    }
}

fn task_uses_schedule(task: &TaskDefinition, schedule_id: &str) -> bool {
    task.triggers.iter().any(|trigger| {
        matches!(
            trigger,
            TaskTrigger::Schedule(trigger) if trigger.schedule == schedule_id
        )
    })
}

fn task_uses_command(task: &TaskDefinition, command_id: &str) -> bool {
    task.actions.iter().any(|action| {
        matches!(
            &action.kind,
            ActionKind::Command(command) if command.command == command_id
        )
    }) || task
        .next
        .iter()
        .any(|rule| rule.command.as_deref() == Some(command_id))
}

fn demo_restart_required(previous: &Config, next: &Config) -> Vec<String> {
    let mut fields = Vec::new();
    if previous.working_dir != next.working_dir {
        fields.push("workingDir".to_string());
    }
    if previous.log != next.log {
        fields.push("log".to_string());
    }
    if previous.http_server != next.http_server {
        fields.push("httpServer".to_string());
    }
    if previous.message_log != next.message_log {
        fields.push("messageLog".to_string());
    }
    fields
}

fn validate_demo_config(config: &Config) -> Result<(), ApiError> {
    let mut errors = Vec::new();
    if config.working_dir.as_os_str().is_empty() {
        errors.push("workingDir must not be empty".to_string());
    }
    if config.log.dir.as_os_str().is_empty() {
        errors.push("log.dir must not be empty".to_string());
    }
    if config.log.file.trim().is_empty() {
        errors.push("log.file must not be empty".to_string());
    }
    if config.http_server.bind.trim().is_empty() {
        errors.push("httpServer.bind must not be empty".to_string());
    }
    if config.http_server.port == 0 {
        errors.push("httpServer.port must be greater than 0".to_string());
    }
    if config.message_log.file.as_os_str().is_empty() {
        errors.push("messageLog.file must not be empty".to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::ConfigInvalid, errors.join("; "))
            .with_details(json!({ "errors": errors })))
    }
}

fn protocol_transport_params(protocol: &str, transport: &str) -> Result<Vec<ParamDef>, ApiError> {
    match (protocol, transport) {
        ("zwave", "serial") => {
            let mut definitions = catalog::serial_params();
            definitions.extend(catalog::zwave_params());
            Ok(definitions)
        }
        ("zigbee", "serial") => {
            let mut definitions = catalog::serial_params();
            for definition in &mut definitions {
                if definition.name == catalog::SERIAL_PARAM_BAUD_RATE {
                    definition.default = Some(json!(57_600));
                }
                if definition.name == catalog::SERIAL_PARAM_FLOW_CONTROL {
                    definition.default = Some(json!("none"));
                }
            }
            definitions.extend(catalog::zigbee_params());
            Ok(definitions)
        }
        ("http", "tcp") => Ok(tcp_transport_info().params),
        (protocol, _) if !matches!(protocol, "zwave" | "zigbee" | "http") => Err(ApiError::new(
            ErrorCode::UnknownProtocol,
            format!("unknown protocol '{protocol}'"),
        )),
        (_, transport) => Err(ApiError::new(
            ErrorCode::UnknownTransport,
            format!("unknown transport '{transport}'"),
        )),
    }
}

fn same_key(service: &ServiceInfo, reference: &ServiceRef) -> bool {
    matches!(
        reference,
        ServiceRef::Key {
            protocol,
            transport,
            entry
        } if service.protocol == *protocol
            && service.transport == *transport
            && service.entry == *entry
    )
}

fn matches_ref(service: &ServiceInfo, reference: &ServiceRef) -> bool {
    match reference {
        ServiceRef::Alias { alias } => service.alias.as_deref() == Some(alias.as_str()),
        ServiceRef::Key { .. } => same_key(service, reference),
    }
}

fn matches_message_ref(message: &Message, reference: &ServiceRef) -> bool {
    match (&message.service, reference) {
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

/// Builds a deterministic demo label set with one of each `extra` label and
/// `prefix-NN` labels in the inclusive range.
fn demo_labels(prefix: &str, first: usize, last: usize, extra: &[&str]) -> Vec<String> {
    let mut labels: Vec<String> = extra.iter().map(|label| (*label).to_string()).collect();
    labels.extend((first..=last).map(|index| format!("{prefix}-{index:02}")));
    labels
}

fn protocol_infos() -> Vec<ProtocolInfo> {
    vec![
        catalog::zwave_protocol_info(),
        catalog::zigbee_protocol_info(),
        http_protocol_info(),
    ]
}

fn http_protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        id: "http".to_string(),
        name: "HTTP".to_string(),
        description: Some("Raw HTTP request/response byte relay".to_string()),
        transports: vec!["tcp".to_string()],
        params: Vec::new(),
    }
}

fn transport_infos() -> Vec<TransportInfo> {
    vec![catalog::serial_transport_info(), tcp_transport_info()]
}

fn tcp_transport_info() -> TransportInfo {
    TransportInfo {
        id: "tcp".to_string(),
        name: "TCP".to_string(),
        description: Some("TCP stream transport".to_string()),
        params: vec![
            ParamDef::with_default(
                "connectTimeout",
                ParamType::Duration,
                json!("10 s"),
                "Connection timeout",
            ),
            ParamDef::with_default(
                "readTimeout",
                ParamType::Duration,
                json!("30 s"),
                "Read timeout",
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, method: &str, params: Value) -> String {
        json!({"id": id, "method": method, "params": params}).to_string()
    }

    fn parse_response(text: &str) -> Value {
        serde_json::from_str(text).expect("response is JSON")
    }

    #[test]
    fn demo_exposes_one_hundred_distinct_labels() {
        let demo = DemoServer::new();
        let labels: std::collections::HashSet<&str> = demo
            .tasks
            .iter()
            .flat_map(|info| info.task.labels.iter().map(String::as_str))
            .collect();
        assert_eq!(labels.len(), 100);
        for info in &demo.tasks {
            hodeco_contract::domain::validate_task_labels(&info.task.labels).unwrap();
            assert!(
                info.task.labels.len() <= hodeco_contract::domain::MAX_TASK_LABELS,
                "{} has too many labels",
                info.task.id
            );
        }
    }

    #[test]
    fn lists_fixture_services_and_messages() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request("1", "services.list", json!({})));
        assert_eq!(responses.len(), 1);
        let response = parse_response(&responses[0]);
        assert_eq!(response["id"], "1");
        assert_eq!(response["result"]["services"].as_array().unwrap().len(), 5);

        let responses = server.handle(&request("2", "messages.list", json!({"limit": 5})));
        let response = parse_response(&responses[0]);
        assert_eq!(response["result"]["messages"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn cancelling_a_demo_run_removes_it_without_history() {
        let mut server = DemoServer::new();
        let run_id = server
            .tasks
            .iter()
            .find(|info| info.task.id == "porch-motion")
            .and_then(|info| info.running_run_id)
            .expect("fixture running run");
        let history_before = server.runs.len();

        let responses = server.handle(&request(
            "1",
            "tasks.cancelRun",
            json!({"taskId": "porch-motion", "runId": run_id}),
        ));
        let response = parse_response(&responses[0]);
        assert!(response["error"].is_null(), "{response}");
        let task = server
            .tasks
            .iter()
            .find(|info| info.task.id == "porch-motion")
            .expect("motion task");
        assert!(!task.running);
        assert_eq!(task.running_run_id, None);
        assert_eq!(server.runs.len(), history_before);

        let response = parse_response(
            &server.handle(&request(
                "2",
                "tasks.history",
                json!({"taskIds": ["porch-motion"]}),
            ))[0],
        );
        let runs = response["result"]["runs"].as_array().unwrap();
        assert!(runs.iter().all(|run| {
            run["id"] != serde_json::json!(run_id) && run["outcome"] != "cancelled"
        }));
    }

    #[test]
    fn run_feed_pages_like_messages_list() {
        let mut server = DemoServer::new();
        let response =
            parse_response(&server.handle(&request("1", "tasks.runFeed", json!({"limit": 1})))[0]);
        let runs = response["result"]["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        let next = response["result"]["next"].clone();
        assert!(next.is_object(), "{response}");
        let response = parse_response(
            &server.handle(&request(
                "2",
                "tasks.runFeed",
                json!({
                    "from": next["from"],
                    "until": next["until"],
                    "limit": 1
                }),
            ))[0],
        );
        assert!(
            response["result"]["runs"]
                .as_array()
                .is_some_and(|runs| !runs.is_empty()),
            "{response}"
        );
    }

    #[test]
    fn schedule_preview_uses_the_draft_schedule() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "schedules.preview",
            json!({
                "schedule": {
                    "id": "third-saturday",
                    "name": "Third Saturday",
                    "timezone": "UTC",
                    "at": "12:00",
                    "days": ["3sat"]
                },
                "limit": 3
            }),
        ));
        let response = parse_response(&responses[0]);
        let occurrences = response["result"]["occurrences"].as_array().unwrap();
        assert_eq!(occurrences.len(), 3);
        for occurrence in occurrences {
            let local = occurrence["local"].as_str().unwrap();
            assert!(local.ends_with("12:00:00 UTC"), "{local}");
        }
    }

    #[test]
    fn demo_exposes_a_non_zwave_protocol_with_large_messages() {
        let server = DemoServer::new();
        assert!(
            server
                .services
                .iter()
                .any(|service| service.protocol == "http" && service.transport == "tcp")
        );
        assert!(
            protocol_infos()
                .iter()
                .any(|protocol| protocol.id == "http")
        );
        assert!(
            transport_infos()
                .iter()
                .any(|transport| transport.id == "tcp")
        );

        let http_messages: Vec<_> = server
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    &message.service,
                    ServiceRef::Alias { alias } if alias == "HTTP"
                )
            })
            .collect();
        assert_eq!(http_messages.len(), 6);
        assert_eq!(
            http_messages
                .iter()
                .filter(|message| message.payload.as_slice().len() >= 100_000)
                .count(),
            2
        );
        assert!(http_messages.iter().any(|message| {
            String::from_utf8_lossy(message.payload.as_slice()).contains("こんにちは")
        }));
        assert!(http_messages.iter().any(|message| {
            String::from_utf8_lossy(message.payload.as_slice()).contains("Привет")
        }));
    }

    #[test]
    fn discovery_runs_and_finishes() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "zwave", "transport": "serial"}),
        ));
        assert_eq!(responses.len(), 2);
        assert!(responses[1].contains("events.discoveryStarted"));

        let output = server.tick();
        assert!(
            output
                .iter()
                .any(|event| event.contains("events.discoveryFinished"))
        );

        let responses = server.handle(&request("2", "discovery.get", json!({})));
        let response = parse_response(&responses[0]);
        assert_eq!(response["result"]["discovery"]["state"], "finished");
        assert_eq!(
            response["result"]["discovery"]["entries"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn zigbee_discovery_returns_ezsp_entries() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "zigbee", "transport": "serial"}),
        ));
        assert!(parse_response(&responses[0])["error"].is_null());
        server.tick();
        let discovery = server.discovery.as_ref().expect("discovery exists");
        assert_eq!(discovery.protocol, "zigbee");
        assert!(discovery.entries.iter().any(|entry| {
            entry.entry == "COM8"
                && entry.available
                && entry
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("EZSP v4"))
        }));
        assert!(discovery.entries.iter().any(|entry| {
            entry.entry == "COM5"
                && !entry.available
                && entry
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("already in use"))
        }));
    }

    #[test]
    fn discovery_entry_can_be_added_to_demo_services() {
        let mut server = DemoServer::new();
        server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "zigbee", "transport": "serial"}),
        ));
        server.tick();
        let discovery = server.discovery.as_ref().expect("discovery exists");
        let entry = discovery
            .entries
            .iter()
            .find(|entry| entry.available)
            .map(|entry| entry.entry.clone())
            .expect("available discovery entry");
        let services_before = server.services.len();

        let response = parse_response(
            &server.handle(&request(
                "2",
                "services.add",
                json!({
                    "alias": "ZB2",
                    "protocol": "zigbee",
                    "transport": "serial",
                    "entry": entry,
                }),
            ))[0],
        );
        assert!(response["error"].is_null(), "{response}");
        assert_eq!(server.services.len(), services_before + 1);
        assert!(
            server
                .services
                .iter()
                .any(|service| service.alias.as_deref() == Some("ZB2"))
        );
    }

    #[test]
    fn http_discovery_returns_http_entries() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "http", "transport": "tcp"}),
        ));
        assert_eq!(responses.len(), 2);
        assert!(responses[1].contains("events.discoveryStarted"));

        server.tick();
        let discovery = server.discovery.as_ref().expect("discovery exists");
        assert_eq!(discovery.protocol, "http");
        assert!(
            discovery
                .entries
                .iter()
                .any(|entry| entry.entry == "localhost:8080")
        );
    }

    #[test]
    fn send_creates_and_promotes_an_outgoing_message() {
        let mut server = DemoServer::new();
        server.handle(&request(
            "1",
            "events.subscribe",
            json!({"topics": ["messages", "discovery", "services"]}),
        ));
        let responses = server.handle(&request(
            "2",
            "services.send",
            json!({"service": {"alias": "ZW"}, "payload": "AQID"}),
        ));
        assert!(responses[0].contains("\"result\""));
        assert!(responses[1].contains("events.messageNew"));

        let output = server.tick();
        assert!(
            output
                .iter()
                .any(|event| event.contains("events.messageStateChanged"))
        );
    }

    #[test]
    fn rejects_unknown_methods_and_services() {
        let mut server = DemoServer::new();
        let response = parse_response(&server.handle("{\"id\":\"1\",\"method\":\"nope\"}")[0]);
        assert_eq!(response["error"]["code"], "unknownMethod");

        let response = parse_response(
            &server.handle(&request(
                "2",
                "services.status",
                json!({"service": {"alias": "missing"}}),
            ))[0],
        );
        assert_eq!(response["error"]["code"], "serviceNotFound");
    }

    #[test]
    fn filters_messages_by_payload_expression_and_ids() {
        let mut server = DemoServer::new();
        let payload = frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x2d]);
        let encoded = Base64Bytes::from(payload).to_string();
        let response = parse_response(
            &server.handle(&request(
                "1",
                "messages.list",
                json!({
                    "payload": {"offset": 0, "bytes": encoded},
                    "states": [0],
                    "order": 1
                }),
            ))[0],
        );
        let messages = response["result"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["state"], serde_json::json!(0));
        let id = messages[0]["id"].as_str().unwrap().to_string();

        let response = parse_response(
            &server.handle(&request(
                "2",
                "messages.list",
                json!({"ids": [id, "00000000-0000-0000-0000-000000000000"]}),
            ))[0],
        );
        assert_eq!(response["result"]["messages"].as_array().unwrap().len(), 1);
        assert!(response["result"]["storeTotal"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn updates_config_and_service_parameters() {
        let mut server = DemoServer::new();
        let mut config = server.config();
        config.http_server.port = 9000;
        let response = parse_response(
            &server.handle(&request("1", "config.update", json!({"config": config})))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(response["result"]["restartRequired"], json!(["httpServer"]));
        assert_eq!(server.config().http_server.port, 9000);

        let response = parse_response(
            &server.handle(&request(
                "2",
                "services.params",
                json!({"service": {"alias": "HTTP"}, "params": {"readTimeout": "45 s"}}),
            ))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(
            response["result"]["service"]["params"]["readTimeout"],
            "45 s"
        );
        let service = server
            .services
            .iter()
            .find(|service| service.alias.as_deref() == Some("HTTP"))
            .expect("HTTP service");
        assert_eq!(service.params["readTimeout"], "45 s");

        let response = parse_response(
            &server.handle(&request(
                "3",
                "protocols.params",
                json!({"protocol": "http", "transport": "tcp"}),
            ))[0],
        );
        assert!(
            response["result"]["params"]
                .as_array()
                .unwrap()
                .iter()
                .any(|definition| definition["name"] == "readTimeout")
        );
    }

    #[test]
    fn demo_catalog_payloads_match_contract_types() {
        use hodeco_contract::api::payloads::{
            ProtocolsListResult, ServicesListResult, TransportsListResult,
        };

        let mut server = DemoServer::new();
        for (method, parse) in [
            (
                "protocols.list",
                (|value| serde_json::from_value::<ProtocolsListResult>(value).map(|_| ()))
                    as fn(serde_json::Value) -> Result<(), _>,
            ),
            (
                "transports.list",
                (|value| serde_json::from_value::<TransportsListResult>(value).map(|_| ()))
                    as fn(serde_json::Value) -> Result<(), _>,
            ),
            (
                "services.list",
                (|value| serde_json::from_value::<ServicesListResult>(value).map(|_| ()))
                    as fn(serde_json::Value) -> Result<(), _>,
            ),
        ] {
            let response = parse_response(&server.handle(&request("1", method, json!({})))[0]);
            parse(response["result"].clone())
                .unwrap_or_else(|error| panic!("{method} result does not match: {error}"));
        }
    }

    #[test]
    fn zigbee_protocol_and_fixtures_are_exposed() {
        let mut server = DemoServer::new();
        let response = parse_response(
            &server.handle(&request(
                "1",
                "protocols.params",
                json!({"protocol": "zigbee", "transport": "serial"}),
            ))[0],
        );
        let params = response["result"]["params"].as_array().unwrap();
        assert!(
            params
                .iter()
                .any(|param| { param["name"] == "baudRate" && param["default"] == json!(57_600) })
        );
        assert!(
            params.iter().any(|param| {
                param["name"] == "flowControl" && param["default"] == json!("none")
            })
        );
        assert!(params.iter().any(|param| param["name"] == "ashAckTimeout"));

        let response = parse_response(&server.handle(&request("2", "services.list", json!({})))[0]);
        let services = response["result"]["services"].as_array().unwrap();
        let zigbee = services
            .iter()
            .find(|service| service["alias"] == "ZB")
            .expect("zigbee fixture");
        assert_eq!(zigbee["protocol"], "zigbee");
        assert_eq!(zigbee["status"]["detail"], "EZSP v4 · stack 0x5410");
    }

    #[test]
    fn zigbee_command_fixtures_decode() {
        let server = DemoServer::new();
        let zigbee = ServiceRef::Alias {
            alias: "ZB".to_string(),
        };
        let decoded: Vec<String> = server
            .messages
            .iter()
            .filter(|message| message.service == zigbee)
            .map(|message| zigbee::decode(message.payload.as_slice()).summary)
            .collect();

        assert!(
            decoded
                .iter()
                .any(|summary| summary.contains("permitJoining"))
        );
        assert!(decoded.iter().any(|summary| summary.contains("level 100")));
        assert!(
            decoded
                .iter()
                .any(|summary| summary.contains("Color Control"))
        );
        assert!(
            decoded
                .iter()
                .any(|summary| summary.contains("Temperature Measurement"))
        );
    }

    #[test]
    fn config_update_does_not_touch_services() {
        let mut server = DemoServer::new();
        let mut config = server.config();
        config.log.file = "updated.log".to_string();

        let response = parse_response(
            &server.handle(&request("1", "config.update", json!({"config": config})))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(response["result"]["restartRequired"], json!(["log"]));
        assert_eq!(server.services.len(), 5);
    }
}
