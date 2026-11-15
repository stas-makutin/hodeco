use crate::config::Config;
use crate::domain::{
    DiscoveryInfo, Message, MessageDirection, MessageOrder, MessageState, ParamDef, PayloadExpr,
    ProtocolInfo, ServiceInfo, ServiceRef, ServiceStatus, TransportInfo,
};
use crate::utils::{Base64Bytes, HumanDuration};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerInfoResult {
    pub version: String,
    pub protocol_version: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolsListResult {
    pub protocols: Vec<ProtocolInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransportsListResult {
    pub transports: Vec<TransportInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolInfoParams {
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolInfoResult {
    pub protocol: ProtocolInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolParamsParams {
    pub protocol: String,
    pub transport: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolParamsResult {
    pub params: Vec<ParamDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigUpdateParams {
    pub config: Config,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigUpdateResult {
    pub config: Config,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_required: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryStartParams {
    pub protocol: String,
    pub transport: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryStartResult {
    pub discovery_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryGetResult {
    pub discovery: DiscoveryInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryCancelParams {
    pub discovery_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesListResult {
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesAddParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesAddResult {
    pub service: ServiceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesRemoveParams {
    pub service: ServiceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesAliasParams {
    pub service: ServiceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesAliasResult {
    pub service: ServiceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesStatusParams {
    pub service: ServiceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesStatusResult {
    pub status: ServiceStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesParamsParams {
    pub service: ServiceRef,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesParamsResult {
    pub service: ServiceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesSendParams {
    pub service: ServiceRef,
    pub payload: Base64Bytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<HumanDuration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesSendResult {
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessagesGetParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessagesGetResult {
    pub message: Message,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessagesListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub order: MessageOrder,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<MessageState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directions: Vec<MessageDirection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transports: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<PayloadExpr>,
}

/// Absolute timestamp bounds for the next page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageTimeRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessagesListResult {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<MessageTimeRange>,
    pub store_total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventTopic {
    Messages,
    Discovery,
    Services,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventsSubscribeParams {
    pub topics: Vec<EventTopic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventsUnsubscribeParams {
    pub topics: Vec<EventTopic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn deserializes_send_params() {
        let params: ServicesSendParams = serde_json::from_value(serde_json::json!({
            "service": {"alias": "ZW"},
            "payload": "AQID",
            "ttl": "10 s"
        }))
        .unwrap();
        assert_eq!(params.payload.as_slice(), &[1, 2, 3]);
        assert_eq!(
            params.ttl,
            Some(HumanDuration::from_std(std::time::Duration::from_secs(10)))
        );
    }

    #[test]
    fn deserializes_list_params() {
        let params: MessagesListParams = serde_json::from_value(serde_json::json!({
            "from": "2026-11-13T18:00:00Z",
            "until": "2026-11-13T19:00:00Z",
            "order": 1,
            "limit": 50,
            "states": [0, 2],
            "services": [{"alias": "ZW"}],
            "ids": ["3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d"],
            "payload": {
                "op": "and",
                "args": [
                    {"offset": 2, "bytes": "AQID"},
                    {"offset": null, "bytes": "BAUG", "mask": "/wAA"}
                ]
            }
        }))
        .unwrap();
        assert_eq!(
            params.from,
            Some(
                DateTime::parse_from_rfc3339("2026-11-13T18:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(params.order, MessageOrder::OldestFirst);
        assert_eq!(
            params.states,
            vec![MessageState::Incoming, MessageState::OutgoingPending]
        );
        assert_eq!(
            params.services,
            vec![ServiceRef::Alias {
                alias: "ZW".to_string()
            }]
        );
        assert_eq!(
            params.ids,
            vec![Uuid::parse_str("3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d").unwrap()]
        );
        let payload = params.payload.expect("payload expression");
        assert!(payload.matches(&[9, 9, 1, 2, 3, 4, 5, 6]));
        assert!(!payload.matches(&[9, 9, 1, 2, 3, 7, 5, 6]));
    }

    #[test]
    fn serializes_topics_and_time_ranges() {
        assert_eq!(
            serde_json::to_value(EventTopic::Messages).unwrap(),
            serde_json::json!("messages")
        );
        assert_eq!(
            serde_json::to_value(EventTopic::Discovery).unwrap(),
            serde_json::json!("discovery")
        );
        let range = MessageTimeRange {
            from: Some(
                DateTime::parse_from_rfc3339("2026-11-09T12:34:56.789Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            until: None,
        };
        assert_eq!(
            serde_json::to_value(range).unwrap(),
            serde_json::json!({"from": "2026-11-09T12:34:56.789Z"})
        );
    }

    #[test]
    fn serializes_empty() {
        assert_eq!(
            serde_json::to_value(Empty {}).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn accepts_config_as_a_result() {
        let config = Config::default();
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["httpServer"]["port"], 8180);
    }

    #[test]
    fn serializes_config_update() {
        let config = Config::default();
        let params: ConfigUpdateParams =
            serde_json::from_value(serde_json::json!({ "config": config })).unwrap();
        assert_eq!(params.config, config);

        let result = ConfigUpdateResult {
            config,
            restart_required: vec!["httpServer".to_string(), "services".to_string()],
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["restartRequired"][0], "httpServer");
        assert_eq!(value["restartRequired"][1], "services");
    }

    #[test]
    fn serializes_protocol_and_service_params() {
        let params: ProtocolParamsParams = serde_json::from_value(serde_json::json!({
            "protocol": "zwave",
            "transport": "serial"
        }))
        .unwrap();
        assert_eq!(params.protocol, "zwave");

        let params: ServicesParamsParams = serde_json::from_value(serde_json::json!({
            "service": {"alias": "ZW"},
            "params": {"baudRate": 57600}
        }))
        .unwrap();
        assert_eq!(params.params["baudRate"], 57_600);
        assert_eq!(
            params.service,
            ServiceRef::Alias {
                alias: "ZW".to_string()
            }
        );
    }
}
