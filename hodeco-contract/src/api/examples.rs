//! Generated parameter examples for the RPC console.
//!
//! Examples are built from the contract DTOs and serialized with serde. This
//! keeps the console in step with field names and optional fields; only the
//! placeholder values (UUID, service alias, payload bytes) are chosen here.

use super::Method;
use super::payloads::{
    ConfigUpdateParams, DiscoveryCancelParams, DiscoveryGetParams, DiscoveryStartParams,
    EventTopic, EventsSubscribeParams, EventsUnsubscribeParams, MessagesGetParams,
    MessagesListParams, ProtocolInfoParams, ProtocolParamsParams, ServicesAddParams,
    ServicesAliasParams, ServicesParamsParams, ServicesRemoveParams, ServicesSendParams,
    ServicesStatusParams,
};
use crate::config::Config;
use crate::domain::ServiceRef;
use crate::utils::{Base64Bytes, HumanDuration};
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
        Method::ConfigUpdate => {
            // Services are managed through services.*; the config.update
            // handler always replaces this field with the current registry.
            let mut value = encode(ConfigUpdateParams {
                config: Config::default(),
            });
            if let Some(config) = value.get_mut("config").and_then(Value::as_object_mut) {
                config.remove("services");
            }
            value
        }
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
        assert_eq!(list["order"], serde_json::json!(0));

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
