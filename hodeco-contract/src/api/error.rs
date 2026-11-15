use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    InvalidRequest,
    UnknownMethod,
    InvalidParams,
    InternalError,
    ConfigInvalid,
    ConfigWriteFailed,
    UnknownProtocol,
    UnknownTransport,
    ProtocolError,
    TransportError,
    ServiceNotFound,
    ServiceAlreadyExists,
    ServiceAliasConflict,
    ServiceNotReady,
    ServiceSendFailed,
    DiscoveryNotFound,
    DiscoveryBusy,
    MessageNotFound,
    ServerShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_error_codes() {
        assert_eq!(
            serde_json::to_value(ErrorCode::ServiceNotFound).unwrap(),
            serde_json::json!("serviceNotFound")
        );
        assert_eq!(
            serde_json::to_value(ErrorCode::ServerShuttingDown).unwrap(),
            serde_json::json!("serverShuttingDown")
        );
    }

    #[test]
    fn serializes_errors() {
        let error = ApiError::new(ErrorCode::ServiceNotFound, "service not found")
            .with_details(serde_json::json!({"alias": "ZW"}));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "code": "serviceNotFound",
                "message": "service not found",
                "details": {"alias": "ZW"}
            })
        );
    }
}
