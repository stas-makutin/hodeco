//! Generated parameter examples for the RPC console.
//!
//! Examples are built from the contract DTOs and serialized with serde. This
//! keeps the console in step with field names and optional fields; only the
//! placeholder values (UUID, service alias, payload bytes) are chosen here.

use super::Method;
use super::payloads::{
    CommandsAddParams, CommandsGetParams, CommandsRemoveParams, CommandsUpdateParams,
    ConfigUpdateParams, DiscoveryCancelParams, DiscoveryGetParams, DiscoveryStartParams,
    EventTopic, EventsSubscribeParams, EventsUnsubscribeParams, MessagesGetParams,
    MessagesListParams, ProtocolInfoParams, ProtocolParamsParams, SchedulesAddParams,
    SchedulesGetParams, SchedulesPreviewParams, SchedulesRemoveParams, SchedulesSetDefaultsParams,
    SchedulesSolarPreviewParams, SchedulesUpdateParams, ServicesAddParams, ServicesAliasParams,
    ServicesParamsParams, ServicesRemoveParams, ServicesSendParams, ServicesStatusParams,
    TasksAddParams, TasksCancelRunParams, TasksFireParams, TasksGetParams, TasksGetRunParams,
    TasksHistoryParams, TasksPurgeHistoryParams, TasksRemoveParams, TasksRunFeedParams,
    TasksUpdateParams,
};
use crate::config::Config;
use crate::domain::{
    AtSpec, CommandDefinition, Location, OneOrMany, ScheduleDefinition, ServiceRef, TaskDefinition,
};
use crate::utils::{Base64Bytes, HumanDuration};
use chrono::NaiveDate;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

/// Returns a JSON object suitable for the console's `params` field.
pub fn example_params(method: Method) -> Value {
    match method {
        Method::SystemRestart
        | Method::ServerInfo
        | Method::ConfigGet
        | Method::ProtocolsList
        | Method::TransportsList
        | Method::ServicesList => json!({}),
        Method::MessagesList => encode(MessagesListParams {
            limit: Some(50),
            ..MessagesListParams::default()
        }),
        Method::ConfigUpdate => encode(ConfigUpdateParams {
            config: Config::default(),
        }),
        Method::ProtocolsInfo => encode(ProtocolInfoParams {
            protocol: "zwave".to_string(),
        }),
        Method::ProtocolsParams => encode(ProtocolParamsParams {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
        }),
        Method::DiscoveryStart => encode(DiscoveryStartParams {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            params: BTreeMap::new(),
        }),
        Method::DiscoveryGet => encode(DiscoveryGetParams {
            discovery_id: Some(Uuid::nil()),
        }),
        Method::DiscoveryCancel => encode(DiscoveryCancelParams {
            discovery_id: Uuid::nil(),
        }),
        Method::ServicesAdd => encode(ServicesAddParams {
            alias: Some("ZW".to_string()),
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM3".to_string(),
            params: BTreeMap::new(),
        }),
        Method::ServicesRemove => encode(ServicesRemoveParams {
            service: service_ref(),
        }),
        Method::ServicesAlias => encode(ServicesAliasParams {
            service: service_ref(),
            alias: Some("ZW2".to_string()),
        }),
        Method::ServicesStatus => encode(ServicesStatusParams {
            service: service_ref(),
        }),
        Method::ServicesParams => encode(ServicesParamsParams {
            service: service_ref(),
            params: BTreeMap::from([("baudRate".to_string(), json!(115_200))]),
        }),
        Method::ServicesSend => encode(ServicesSendParams {
            service: service_ref(),
            payload: Base64Bytes::from(vec![0x01, 0x03, 0x00, 0x15]),
            ttl: Some(HumanDuration::from_std(Duration::from_secs(10))),
        }),
        Method::MessagesGet => encode(MessagesGetParams { id: Uuid::nil() }),
        Method::TasksList => json!({}),
        Method::TasksGet => encode(TasksGetParams {
            task_id: "evening-lights".to_string(),
        }),
        Method::TasksGetRun => encode(TasksGetRunParams {
            task_id: "evening-lights".to_string(),
            run_id: Uuid::nil(),
        }),
        Method::TasksAdd => encode(TasksAddParams {
            task: TaskDefinition {
                id: "evening-lights".to_string(),
                name: "Evening lights".to_string(),
                ..TaskDefinition::default()
            },
        }),
        Method::TasksUpdate => encode(TasksUpdateParams {
            task_id: "evening-lights".to_string(),
            task: TaskDefinition {
                id: "evening-lights".to_string(),
                name: "Evening lights".to_string(),
                ..TaskDefinition::default()
            },
        }),
        Method::TasksRemove => encode(TasksRemoveParams {
            task_id: "evening-lights".to_string(),
        }),
        Method::TasksFire => encode(TasksFireParams {
            task_id: "evening-lights".to_string(),
            payload: Some(Base64Bytes::from(vec![1, 2, 3])),
            ..TasksFireParams::default()
        }),
        Method::TasksCancelRun => encode(TasksCancelRunParams {
            task_id: "evening-lights".to_string(),
            run_id: Uuid::nil(),
        }),
        Method::TasksHistory => encode(TasksHistoryParams {
            limit: Some(50),
            ..TasksHistoryParams::default()
        }),
        Method::TasksRunFeed => encode(TasksRunFeedParams {
            limit: Some(50),
            ..TasksRunFeedParams::default()
        }),
        Method::TasksPurgeHistory => encode(TasksPurgeHistoryParams {
            task_id: "evening-lights".to_string(),
        }),
        Method::SchedulesList => json!({}),
        Method::SchedulesGet => encode(SchedulesGetParams {
            schedule_id: "before-sunset".to_string(),
        }),
        Method::SchedulesAdd => encode(SchedulesAddParams {
            schedule: ScheduleDefinition {
                id: "before-sunset".to_string(),
                name: "Before sunset".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                repeat_at: OneOrMany::One(HumanDuration::from_std(Duration::from_secs(30 * 60))),
                ..ScheduleDefinition::default()
            },
        }),
        Method::SchedulesUpdate => encode(SchedulesUpdateParams {
            schedule_id: "before-sunset".to_string(),
            schedule: ScheduleDefinition {
                id: "before-sunset".to_string(),
                name: "Before sunset".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                repeat_at: OneOrMany::One(HumanDuration::from_std(Duration::from_secs(30 * 60))),
                ..ScheduleDefinition::default()
            },
        }),
        Method::SchedulesRemove => encode(SchedulesRemoveParams {
            schedule_id: "before-sunset".to_string(),
        }),
        Method::SchedulesPreview => encode(SchedulesPreviewParams {
            schedule: ScheduleDefinition {
                id: "before-sunset".to_string(),
                name: "Before sunset".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                repeat_at: OneOrMany::One(HumanDuration::from_std(Duration::from_secs(30 * 60))),
                ..ScheduleDefinition::default()
            },
            limit: Some(20),
        }),
        Method::SchedulesSetDefaults => encode(SchedulesSetDefaultsParams {
            location: Some(Location::new(52.52, 13.405, Some(34.0))),
            timezone: Some("Europe/Berlin".to_string()),
        }),
        Method::SchedulesSolarPreview => encode(SchedulesSolarPreviewParams {
            location: Location::new(52.52, 13.405, Some(34.0)),
            date: NaiveDate::from_ymd_opt(2026, 6, 21).unwrap(),
            timezone: Some("Europe/Berlin".to_string()),
        }),
        Method::CommandsList => json!({}),
        Method::CommandsGet => encode(CommandsGetParams {
            command_id: "meter".to_string(),
        }),
        Method::CommandsAdd => encode(CommandsAddParams {
            command: CommandDefinition {
                id: "meter".to_string(),
                name: "Meter poll".to_string(),
                executable: std::path::PathBuf::from("/usr/local/bin/meter-poll"),
                ..CommandDefinition::default()
            },
        }),
        Method::CommandsUpdate => encode(CommandsUpdateParams {
            command_id: "meter".to_string(),
            command: CommandDefinition {
                id: "meter".to_string(),
                name: "Meter poll".to_string(),
                executable: std::path::PathBuf::from("/usr/local/bin/meter-poll"),
                ..CommandDefinition::default()
            },
        }),
        Method::CommandsRemove => encode(CommandsRemoveParams {
            command_id: "meter".to_string(),
        }),
        Method::EventsSubscribe => encode(EventsSubscribeParams {
            topics: vec![EventTopic::Messages],
        }),
        Method::EventsUnsubscribe => encode(EventsUnsubscribeParams {
            topics: vec![EventTopic::Messages],
        }),
    }
}

fn service_ref() -> ServiceRef {
    ServiceRef::Alias {
        alias: "ZW".to_string(),
    }
}

fn encode<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_has_an_object_example() {
        for method in Method::ALL {
            let example = example_params(method);
            assert!(
                example.is_object(),
                "{method} example is not an object: {example}"
            );
        }
    }

    #[test]
    fn examples_use_contract_field_names() {
        let list = example_params(Method::MessagesList);
        assert_eq!(list["limit"], 50);
        assert!(list.get("order").is_none());

        let send = example_params(Method::ServicesSend);
        assert_eq!(send["service"]["alias"], "ZW");
        assert_eq!(send["payload"], "AQMAFQ==");
        assert_eq!(send["ttl"], "10 s");

        let subscribe = example_params(Method::EventsSubscribe);
        assert_eq!(subscribe["topics"], json!(["messages"]));

        let discovery = example_params(Method::DiscoveryGet);
        assert_eq!(
            discovery["discoveryId"],
            "00000000-0000-0000-0000-000000000000"
        );

        let config = example_params(Method::ConfigUpdate);
        assert!(config["config"]["services"].is_null());
        assert!(config["config"]["httpServer"].is_object());
    }
}
