use super::error::{ApiError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "system.restart")]
    SystemRestart,
    #[serde(rename = "server.info")]
    ServerInfo,
    #[serde(rename = "config.get")]
    ConfigGet,
    #[serde(rename = "config.update")]
    ConfigUpdate,
    #[serde(rename = "protocols.list")]
    ProtocolsList,
    #[serde(rename = "transports.list")]
    TransportsList,
    #[serde(rename = "protocols.info")]
    ProtocolsInfo,
    #[serde(rename = "protocols.params")]
    ProtocolsParams,
    #[serde(rename = "discovery.start")]
    DiscoveryStart,
    #[serde(rename = "discovery.get")]
    DiscoveryGet,
    #[serde(rename = "discovery.cancel")]
    DiscoveryCancel,
    #[serde(rename = "services.list")]
    ServicesList,
    #[serde(rename = "services.add")]
    ServicesAdd,
    #[serde(rename = "services.remove")]
    ServicesRemove,
    #[serde(rename = "services.alias")]
    ServicesAlias,
    #[serde(rename = "services.status")]
    ServicesStatus,
    #[serde(rename = "services.params")]
    ServicesParams,
    #[serde(rename = "services.send")]
    ServicesSend,
    #[serde(rename = "messages.get")]
    MessagesGet,
    #[serde(rename = "messages.list")]
    MessagesList,
    #[serde(rename = "events.subscribe")]
    EventsSubscribe,
    #[serde(rename = "events.unsubscribe")]
    EventsUnsubscribe,
}

impl Method {
    pub const ALL: [Self; 22] = [
        Self::SystemRestart,
        Self::ServerInfo,
        Self::ConfigGet,
        Self::ConfigUpdate,
        Self::ProtocolsList,
        Self::TransportsList,
        Self::ProtocolsInfo,
        Self::ProtocolsParams,
        Self::DiscoveryStart,
        Self::DiscoveryGet,
        Self::DiscoveryCancel,
        Self::ServicesList,
        Self::ServicesAdd,
        Self::ServicesRemove,
        Self::ServicesAlias,
        Self::ServicesStatus,
        Self::ServicesParams,
        Self::ServicesSend,
        Self::MessagesGet,
        Self::MessagesList,
        Self::EventsSubscribe,
        Self::EventsUnsubscribe,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemRestart => "system.restart",
            Self::ServerInfo => "server.info",
            Self::ConfigGet => "config.get",
            Self::ConfigUpdate => "config.update",
            Self::ProtocolsList => "protocols.list",
            Self::TransportsList => "transports.list",
            Self::ProtocolsInfo => "protocols.info",
            Self::ProtocolsParams => "protocols.params",
            Self::DiscoveryStart => "discovery.start",
            Self::DiscoveryGet => "discovery.get",
            Self::DiscoveryCancel => "discovery.cancel",
            Self::ServicesList => "services.list",
            Self::ServicesAdd => "services.add",
            Self::ServicesRemove => "services.remove",
            Self::ServicesAlias => "services.alias",
            Self::ServicesStatus => "services.status",
            Self::ServicesParams => "services.params",
            Self::ServicesSend => "services.send",
            Self::MessagesGet => "messages.get",
            Self::MessagesList => "messages.list",
            Self::EventsSubscribe => "events.subscribe",
            Self::EventsUnsubscribe => "events.unsubscribe",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Method {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::UnknownMethod,
                    format!("unknown method '{value}'"),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_method_names() {
        for method in Method::ALL {
            assert_eq!(
                serde_json::to_value(method).unwrap(),
                serde_json::json!(method.as_str())
            );
            assert_eq!(method.as_str().parse::<Method>().unwrap(), method);
        }
    }

    #[test]
    fn rejects_unknown_methods() {
        let error = "unknown.method".parse::<Method>().unwrap_err();
        assert_eq!(error.code, ErrorCode::UnknownMethod);
    }
}
