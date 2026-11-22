use crate::api::payloads::TaskRunNotification;
use crate::domain::{DiscoveryInfo, Message, RunSummary, ServiceRef, ServiceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NotificationMethod {
    #[serde(rename = "events.discoveryStarted")]
    DiscoveryStarted,
    #[serde(rename = "events.discoveryFinished")]
    DiscoveryFinished,
    #[serde(rename = "events.messageNew")]
    MessageNew,
    #[serde(rename = "events.messageDropped")]
    MessageDropped,
    #[serde(rename = "events.messageStateChanged")]
    MessageStateChanged,
    #[serde(rename = "events.serviceStateChanged")]
    ServiceStateChanged,
    #[serde(rename = "events.taskRunStarted")]
    TaskRunStarted,
    #[serde(rename = "events.taskRunFinished")]
    TaskRunFinished,
    #[serde(rename = "events.taskRunDropped")]
    TaskRunDropped,
}

impl NotificationMethod {
    pub const ALL: [Self; 9] = [
        Self::DiscoveryStarted,
        Self::DiscoveryFinished,
        Self::MessageNew,
        Self::MessageDropped,
        Self::MessageStateChanged,
        Self::ServiceStateChanged,
        Self::TaskRunStarted,
        Self::TaskRunFinished,
        Self::TaskRunDropped,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiscoveryStarted => "events.discoveryStarted",
            Self::DiscoveryFinished => "events.discoveryFinished",
            Self::MessageNew => "events.messageNew",
            Self::MessageDropped => "events.messageDropped",
            Self::MessageStateChanged => "events.messageStateChanged",
            Self::ServiceStateChanged => "events.serviceStateChanged",
            Self::TaskRunStarted => "events.taskRunStarted",
            Self::TaskRunFinished => "events.taskRunFinished",
            Self::TaskRunDropped => "events.taskRunDropped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    #[serde(rename = "events.discoveryStarted")]
    DiscoveryStarted(DiscoveryNotification),
    #[serde(rename = "events.discoveryFinished")]
    DiscoveryFinished(DiscoveryNotification),
    #[serde(rename = "events.messageNew")]
    MessageNew(MessageNotification),
    #[serde(rename = "events.messageDropped")]
    MessageDropped(MessageDroppedNotification),
    #[serde(rename = "events.messageStateChanged")]
    MessageStateChanged(MessageNotification),
    #[serde(rename = "events.serviceStateChanged")]
    ServiceStateChanged(ServiceStateChangedNotification),
    #[serde(rename = "events.taskRunStarted")]
    TaskRunStarted(TaskRunNotification),
    #[serde(rename = "events.taskRunFinished")]
    TaskRunFinished(TaskRunFinishedNotification),
    #[serde(rename = "events.taskRunDropped")]
    TaskRunDropped(TaskRunDroppedNotification),
}

impl Notification {
    pub const fn method(&self) -> NotificationMethod {
        match self {
            Self::DiscoveryStarted(_) => NotificationMethod::DiscoveryStarted,
            Self::DiscoveryFinished(_) => NotificationMethod::DiscoveryFinished,
            Self::MessageNew(_) => NotificationMethod::MessageNew,
            Self::MessageDropped(_) => NotificationMethod::MessageDropped,
            Self::MessageStateChanged(_) => NotificationMethod::MessageStateChanged,
            Self::ServiceStateChanged(_) => NotificationMethod::ServiceStateChanged,
            Self::TaskRunStarted(_) => NotificationMethod::TaskRunStarted,
            Self::TaskRunFinished(_) => NotificationMethod::TaskRunFinished,
            Self::TaskRunDropped(_) => NotificationMethod::TaskRunDropped,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DiscoveryNotification {
    pub discovery: DiscoveryInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageNotification {
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageDroppedNotification {
    pub message_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceStateChangedNotification {
    pub service: ServiceRef,
    pub status: ServiceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskRunFinishedNotification {
    pub run: RunSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskRunDroppedNotification {
    pub run_ids: Vec<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MessageState;
    use crate::utils::Base64Bytes;
    use chrono::{DateTime, Utc};

    fn message() -> Message {
        Message {
            id: Uuid::parse_str("3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d").unwrap(),
            time: DateTime::parse_from_rfc3339("2026-11-09T12:34:56.789Z")
                .unwrap()
                .with_timezone(&Utc),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            state: MessageState::Incoming,
            payload: Base64Bytes::from(vec![1, 2, 3]),
        }
    }

    #[test]
    fn serializes_message_notifications() {
        let notification = Notification::MessageNew(MessageNotification { message: message() });
        let value = serde_json::to_value(&notification).unwrap();
        assert_eq!(value["method"], "events.messageNew");
        assert_eq!(value["params"]["message"]["payload"], "AQID");
        assert_eq!(notification.method(), NotificationMethod::MessageNew);
    }

    #[test]
    fn serializes_dropped_notifications() {
        let notification = Notification::MessageDropped(MessageDroppedNotification {
            message_ids: vec![Uuid::parse_str("3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d").unwrap()],
        });
        let value = serde_json::to_value(&notification).unwrap();
        assert_eq!(value["method"], "events.messageDropped");
        assert_eq!(
            value["params"]["messageIds"],
            serde_json::json!(["3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d"])
        );
    }

    #[test]
    fn notification_method_names_match_serialization() {
        for method in NotificationMethod::ALL {
            assert_eq!(
                serde_json::to_value(method).unwrap(),
                serde_json::json!(method.as_str())
            );
        }
    }
}
