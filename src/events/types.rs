use crate::messages::StoreEvent;
use crate::services::discovery::DiscoveryEvent;
use hodeco_contract::api::notifications::TaskRunFinishedNotification;
use hodeco_contract::api::payloads::TaskRunNotification;
use hodeco_contract::domain::{DiscoveryInfo, Message, ServiceRef, ServiceStatus};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Topic {
    Messages,
    Discovery,
    Services,
    Tasks,
}

impl Topic {
    pub const ALL: [Self; 4] = [Self::Messages, Self::Discovery, Self::Services, Self::Tasks];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::Discovery => "discovery",
            Self::Services => "services",
            Self::Tasks => "tasks",
        }
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Topic {
    type Err = UnknownTopic;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|topic| topic.as_str() == value)
            .ok_or_else(|| UnknownTopic(value.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown event topic '{0}'")]
pub struct UnknownTopic(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    DiscoveryStarted(DiscoveryInfo),
    DiscoveryFinished(DiscoveryInfo),
    MessageNew(Message),
    MessageDropped {
        message_ids: Vec<Uuid>,
    },
    MessageStateChanged(Message),
    ServiceStateChanged {
        service: ServiceRef,
        status: ServiceStatus,
    },
    TaskRunStarted(TaskRunNotification),
    TaskRunFinished(TaskRunFinishedNotification),
}

impl Event {
    pub fn topic(&self) -> Topic {
        match self {
            Self::DiscoveryStarted(_) | Self::DiscoveryFinished(_) => Topic::Discovery,
            Self::MessageNew(_) | Self::MessageDropped { .. } | Self::MessageStateChanged(_) => {
                Topic::Messages
            }
            Self::ServiceStateChanged { .. } => Topic::Services,
            Self::TaskRunStarted(_) | Self::TaskRunFinished(_) => Topic::Tasks,
        }
    }
}

impl From<StoreEvent> for Event {
    fn from(event: StoreEvent) -> Self {
        match event {
            StoreEvent::New { message, .. } => Self::MessageNew(message),
            StoreEvent::StateChanged { message, .. } => Self::MessageStateChanged(message),
            StoreEvent::Dropped { message_ids } => Self::MessageDropped { message_ids },
        }
    }
}

impl From<DiscoveryEvent> for Event {
    fn from(event: DiscoveryEvent) -> Self {
        match event {
            DiscoveryEvent::Started(info) => Self::DiscoveryStarted(info),
            DiscoveryEvent::Finished(info) => Self::DiscoveryFinished(info),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use hodeco_contract::domain::{DiscoveryState, MessageState};
    use hodeco_contract::utils::Base64Bytes;

    fn message() -> Message {
        Message {
            id: Uuid::new_v4(),
            time: Utc::now(),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![1]),
        }
    }

    #[test]
    fn parses_topics() {
        for topic in Topic::ALL {
            assert_eq!(topic.as_str().parse::<Topic>().unwrap(), topic);
        }
        assert_eq!(
            "unknown".parse::<Topic>(),
            Err(UnknownTopic("unknown".to_string()))
        );
    }

    #[test]
    fn maps_store_events() {
        let key = hodeco_contract::domain::ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM4".to_string(),
        };
        assert_eq!(
            Event::from(StoreEvent::New {
                key: key.clone(),
                message: message()
            })
            .topic(),
            Topic::Messages
        );
        assert_eq!(
            Event::from(StoreEvent::StateChanged {
                key,
                message: message()
            })
            .topic(),
            Topic::Messages
        );
        assert_eq!(
            Event::from(StoreEvent::Dropped {
                message_ids: vec![Uuid::new_v4()],
            })
            .topic(),
            Topic::Messages
        );
    }

    #[test]
    fn maps_discovery_events() {
        let info = DiscoveryInfo {
            id: Uuid::new_v4(),
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            state: DiscoveryState::Running,
            entries: Vec::new(),
        };
        assert_eq!(
            Event::from(DiscoveryEvent::Started(info.clone())).topic(),
            Topic::Discovery
        );
        assert_eq!(
            Event::from(DiscoveryEvent::Finished(info)).topic(),
            Topic::Discovery
        );
    }
}
