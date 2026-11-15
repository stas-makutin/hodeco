use super::error::{ApiError, ErrorCode};
use super::methods::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub method: Method,
    #[serde(default = "empty_object")]
    pub params: Value,
}

impl Request {
    pub fn new(id: impl Into<String>, method: Method, params: Value) -> Self {
        Self {
            id: id.into(),
            method,
            params,
        }
    }

    pub fn params<T>(&self) -> Result<T, ApiError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_value(self.params.clone()).map_err(|error| {
            ApiError::new(
                ErrorCode::InvalidParams,
                format!("invalid params for '{}': {error}", self.method),
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessResponse {
    pub id: String,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ApiError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Success(SuccessResponse),
    Error(ErrorResponse),
}

impl Response {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self::Success(SuccessResponse {
            id: id.into(),
            result,
        })
    }

    pub fn error(id: impl Into<String>, error: ApiError) -> Self {
        Self::Error(ErrorResponse {
            id: id.into(),
            error,
        })
    }
}

pub fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_requests_without_params() {
        let request: Request =
            serde_json::from_str(r#"{"id":"1","method":"services.list"}"#).unwrap();
        assert_eq!(request.id, "1");
        assert_eq!(request.method, Method::ServicesList);
        assert_eq!(request.params, serde_json::json!({}));
    }

    #[test]
    fn deserializes_requests_with_params() {
        let request: Request = serde_json::from_str(
            r#"{"id":"2","method":"services.send","params":{"service":{"alias":"ZW"},"payload":"AQID"}}"#,
        )
        .unwrap();
        assert_eq!(request.method, Method::ServicesSend);
        assert_eq!(request.params["payload"], "AQID");
    }

    #[test]
    fn rejects_unknown_methods_and_fields() {
        assert!(
            serde_json::from_str::<Request>(r#"{"id":"1","method":"unknown.method"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"id":"1","method":"services.list","unknown":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn serializes_success_responses() {
        let response = Response::success("1", serde_json::json!({"services": []}));
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({"id": "1", "result": {"services": []}})
        );
    }

    #[test]
    fn serializes_error_responses() {
        let response = Response::error(
            "2",
            ApiError::new(ErrorCode::ServiceNotFound, "service not found"),
        );
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({
                "id": "2",
                "error": {"code": "serviceNotFound", "message": "service not found"}
            })
        );
    }

    #[test]
    fn deserializes_responses() {
        let success: Response =
            serde_json::from_str(r#"{"id":"1","result":{"services":[]}}"#).unwrap();
        assert!(matches!(success, Response::Success(_)));

        let error: Response = serde_json::from_str(
            r#"{"id":"2","error":{"code":"serviceNotFound","message":"missing"}}"#,
        )
        .unwrap();
        assert!(matches!(error, Response::Error(_)));
    }

    #[test]
    fn extracts_typed_params() {
        let request = Request::new(
            "1",
            Method::ServicesSend,
            serde_json::json!({"service": {"alias": "ZW"}, "payload": "AQID"}),
        );
        let params: crate::api::payloads::ServicesSendParams = request.params().unwrap();
        assert_eq!(params.payload.as_slice(), &[1, 2, 3]);
    }
}
