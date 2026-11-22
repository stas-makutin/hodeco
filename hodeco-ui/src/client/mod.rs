//! Gateway connection, RPC envelopes, and the WebSocket transport.

pub mod endpoint;
pub mod gateway;
pub mod transport;

use hodeco_contract::api::Response;
use hodeco_contract::api::notifications::Notification;
use serde_json::Value;

pub use gateway::{
    Gateway, GatewayEvent, MessagePageKind, Phase, RequestTag, RunPageKind, backoff,
};
pub use transport::{LiveTransport, Transport, TransportEvent, Wake};

/// A message received on the WebSocket, classified by shape.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response(Response),
    Notification(Notification),
    Unknown(Value),
}

/// Classifies one WebSocket text message. Requests never arrive on this socket,
/// so a message is either an RPC response (has `id`), a notification (has
/// `method`), or something we do not understand and should keep in the log.
pub fn classify(text: &str) -> Result<Incoming, serde_json::Error> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("id").is_some() {
        serde_json::from_value(value).map(Incoming::Response)
    } else if value.get("method").is_some() {
        serde_json::from_value(value).map(Incoming::Notification)
    } else {
        Ok(Incoming::Unknown(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_success_and_error_responses() {
        let incoming = classify(r#"{"id":"ui-1-2","result":{"ok":true}}"#).unwrap();
        assert!(matches!(incoming, Incoming::Response(Response::Success(_))));

        let incoming =
            classify(r#"{"id":"ui-1-2","error":{"code":"internalError","message":"boom"}}"#)
                .unwrap();
        assert!(matches!(incoming, Incoming::Response(Response::Error(_))));
    }

    #[test]
    fn classifies_notifications() {
        let incoming = classify(
            r#"{"method":"events.discoveryFinished","params":{"discovery":{"id":"3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d","protocol":"zwave","transport":"serial","state":"finished"}}}"#,
        )
        .unwrap();
        assert!(matches!(incoming, Incoming::Notification(_)));
    }

    #[test]
    fn keeps_unknown_messages() {
        let incoming = classify(r#"{"hello":"world"}"#).unwrap();
        assert!(matches!(incoming, Incoming::Unknown(_)));
        assert!(classify("not json").is_err());
    }
}
