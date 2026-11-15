use super::service::{ServiceKey, ServiceRef};
use crate::utils::Base64Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
#[repr(u8)]
pub enum MessageState {
    Incoming = 0,
    Outgoing = 1,
    OutgoingPending = 2,
    OutgoingFailed = 3,
    OutgoingRejected = 4,
    OutgoingTimedOut = 5,
}

impl From<MessageState> for u8 {
    fn from(state: MessageState) -> Self {
        state as Self
    }
}

impl TryFrom<u8> for MessageState {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Incoming),
            1 => Ok(Self::Outgoing),
            2 => Ok(Self::OutgoingPending),
            3 => Ok(Self::OutgoingFailed),
            4 => Ok(Self::OutgoingRejected),
            5 => Ok(Self::OutgoingTimedOut),
            other => Err(format!("invalid message state code {other}")),
        }
    }
}

impl MessageState {
    pub fn direction(self) -> MessageDirection {
        match self {
            Self::Incoming => MessageDirection::Incoming,
            Self::Outgoing
            | Self::OutgoingPending
            | Self::OutgoingFailed
            | Self::OutgoingRejected
            | Self::OutgoingTimedOut => MessageDirection::Outgoing,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
#[repr(u8)]
pub enum MessageOrder {
    #[default]
    NewestFirst = 0,
    OldestFirst = 1,
}

impl From<MessageOrder> for u8 {
    fn from(order: MessageOrder) -> Self {
        order as Self
    }
}

impl TryFrom<u8> for MessageOrder {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::NewestFirst),
            1 => Ok(Self::OldestFirst),
            other => Err(format!("invalid message order code {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
#[repr(u8)]
pub enum MessageDirection {
    Incoming = 0,
    Outgoing = 1,
}

impl From<MessageDirection> for u8 {
    fn from(direction: MessageDirection) -> Self {
        direction as Self
    }
}

impl TryFrom<u8> for MessageDirection {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Incoming),
            1 => Ok(Self::Outgoing),
            other => Err(format!("invalid message direction code {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Message {
    pub id: Uuid,
    pub time: DateTime<Utc>,
    pub service: ServiceRef,
    pub state: MessageState,
    pub payload: Base64Bytes,
}

impl Message {
    pub fn from_key(
        id: Uuid,
        time: DateTime<Utc>,
        service: &ServiceKey,
        state: MessageState,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            id,
            time,
            service: ServiceRef::from(service),
            state,
            payload: Base64Bytes::from(payload),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_numeric_code<T>(code: u8, value: T, invalid: u8)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug + Copy,
    {
        let encoded = serde_json::to_value(value).unwrap();
        assert_eq!(encoded, serde_json::json!(code));
        let decoded: T = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, value);
        assert!(serde_json::from_value::<T>(serde_json::json!(invalid)).is_err());
    }

    #[test]
    fn serializes_message_enums_as_stable_numeric_codes() {
        for (code, state) in [
            (0, MessageState::Incoming),
            (1, MessageState::Outgoing),
            (2, MessageState::OutgoingPending),
            (3, MessageState::OutgoingFailed),
            (4, MessageState::OutgoingRejected),
            (5, MessageState::OutgoingTimedOut),
        ] {
            assert_numeric_code(code, state, 6);
        }
        assert_numeric_code(0, MessageDirection::Incoming, 2);
        assert_numeric_code(1, MessageDirection::Outgoing, 2);
        assert_numeric_code(0, MessageOrder::NewestFirst, 2);
        assert_numeric_code(1, MessageOrder::OldestFirst, 2);
    }

    #[test]
    fn serializes_messages() {
        let message = Message {
            id: Uuid::parse_str("3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d").unwrap(),
            time: DateTime::parse_from_rfc3339("2026-11-09T12:34:56.789Z")
                .unwrap()
                .with_timezone(&Utc),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            state: MessageState::OutgoingPending,
            payload: Base64Bytes::from(vec![1, 2, 3]),
        };

        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["id"], "3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d");
        assert_eq!(value["time"], "2026-11-09T12:34:56.789Z");
        assert_eq!(value["service"], serde_json::json!({"alias": "ZW"}));
        assert_eq!(value["state"], serde_json::json!(2));
        assert_eq!(value["payload"], "AQID");

        let reparsed: Message = serde_json::from_value(value).unwrap();
        assert_eq!(reparsed, message);
    }
}
