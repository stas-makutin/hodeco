use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(untagged)]
pub enum ServiceRef {
    Alias {
        alias: String,
    },
    Key {
        protocol: String,
        transport: String,
        entry: String,
    },
}

impl<'de> Deserialize<'de> for ServiceRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Repr {
            alias: Option<String>,
            protocol: Option<String>,
            transport: Option<String>,
            entry: Option<String>,
        }

        let repr = Repr::deserialize(deserializer)?;
        match (repr.alias, repr.protocol, repr.transport, repr.entry) {
            (Some(alias), None, None, None) if !alias.is_empty() => Ok(Self::Alias { alias }),
            (None, Some(protocol), Some(transport), Some(entry))
                if !protocol.is_empty() && !transport.is_empty() && !entry.is_empty() =>
            {
                Ok(Self::Key {
                    protocol,
                    transport,
                    entry,
                })
            }
            _ => Err(de::Error::custom(
                "service reference must contain either 'alias' or 'protocol', 'transport', and 'entry'",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceKey {
    pub protocol: String,
    pub transport: String,
    pub entry: String,
}

impl From<ServiceKey> for ServiceRef {
    fn from(key: ServiceKey) -> Self {
        Self::Key {
            protocol: key.protocol,
            transport: key.transport,
            entry: key.entry,
        }
    }
}

impl From<&ServiceKey> for ServiceRef {
    fn from(key: &ServiceKey) -> Self {
        Self::Key {
            protocol: key.protocol.clone(),
            transport: key.transport.clone(),
            entry: key.entry.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceStatus {
    pub state: ServiceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional protocol-specific detail, e.g. `EZSP v4 · stack 0x5410`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    pub status: ServiceStatus,
    /// Explicit overrides only. Missing values inherit protocol/transport
    /// defaults resolved by the backend.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_service_refs() {
        let alias = ServiceRef::Alias {
            alias: "ZW".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&alias).unwrap(),
            serde_json::json!({"alias": "ZW"})
        );

        let key = ServiceRef::Key {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM3".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&key).unwrap(),
            serde_json::json!({
                "protocol": "zwave",
                "transport": "serial",
                "entry": "COM3"
            })
        );
    }

    #[test]
    fn deserializes_service_refs() {
        assert_eq!(
            serde_json::from_value::<ServiceRef>(serde_json::json!({"alias": "ZW"})).unwrap(),
            ServiceRef::Alias {
                alias: "ZW".to_string()
            }
        );
        assert_eq!(
            serde_json::from_value::<ServiceRef>(serde_json::json!({
                "protocol": "zwave",
                "transport": "serial",
                "entry": "COM3"
            }))
            .unwrap(),
            ServiceRef::Key {
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM3".to_string()
            }
        );
    }

    #[test]
    fn rejects_ambiguous_or_incomplete_service_refs() {
        assert!(
            serde_json::from_value::<ServiceRef>(serde_json::json!({
                "alias": "ZW",
                "protocol": "zwave",
                "transport": "serial",
                "entry": "COM3"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ServiceRef>(serde_json::json!({
                "protocol": "zwave",
                "transport": "serial"
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<ServiceRef>(serde_json::json!({"alias": ""})).is_err());
        assert!(
            serde_json::from_value::<ServiceRef>(serde_json::json!({"alias": "ZW", "extra": 1}))
                .is_err()
        );
    }

    #[test]
    fn serializes_service_states() {
        assert_eq!(
            serde_json::to_value(ServiceState::Running).unwrap(),
            serde_json::json!("running")
        );
    }
}
