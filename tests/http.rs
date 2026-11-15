use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hodeco::app::AppState;
use hodeco::config::ConfigStore;
use hodeco::events::{self, Event};
use hodeco::http;
use hodeco::messages::MessageStore;
use hodeco::services::ServiceError;
use hodeco::services::discovery::{DiscoveryBackend, DiscoveryRegistry};
use hodeco::services::registry::ServiceRegistry;
use hodeco_contract::config::Config;
use hodeco_contract::domain::{DiscoveryEntry, MessageState, ParamValues, ServiceKey};
use hodeco_contract::utils::HumanDuration;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct EmptyDiscovery;

impl DiscoveryBackend for EmptyDiscovery {
    fn discover(
        &self,
        _protocol: String,
        _transport: String,
        _params: ParamValues,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryEntry>, ServiceError>> + Send + 'static>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn app(config: Config) -> (AppState, CancellationToken) {
    app_with_config_store(ConfigStore::new(None, config))
}

fn app_with_config_path(config: Config, path: PathBuf) -> (AppState, CancellationToken) {
    app_with_config_store(ConfigStore::new(Some(path), config))
}

fn app_with_config_store(config_store: ConfigStore) -> (AppState, CancellationToken) {
    let cancel = CancellationToken::new();
    let store = Arc::new(MessageStore::new(1024 * 1024));
    let registry = Arc::new(ServiceRegistry::with_defaults(store.clone(), &cancel));
    let discovery = Arc::new(DiscoveryRegistry::new(Arc::new(EmptyDiscovery), &cancel));
    let (events, _) = events::channel();
    let (resync, _) = tokio::sync::broadcast::channel(1);
    let config = config_store.snapshot();
    let ws_limit = config.http_server.max_connections.max(1) as usize;
    let state = AppState {
        config: Arc::new(config),
        config_store: Arc::new(config_store),
        registry,
        discovery,
        store,
        events,
        resync,
        cancel: cancel.clone(),
        shutting_down: Arc::new(AtomicBool::new(false)),
        ws_limits: Arc::new(tokio::sync::Semaphore::new(ws_limit)),
        started_at: Utc::now(),
    };
    (state, cancel)
}

async fn send(state: &AppState, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = http::router(state.clone())
        .oneshot(request)
        .await
        .expect("router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body collection")
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

fn rpc_request(body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.into())
        .unwrap()
}

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("JSON response")
}

#[tokio::test]
async fn health_reports_ok() {
    let (state, _cancel) = app(Config::default());
    let request = Request::builder()
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();

    let (status, _headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body), json!({"status": "ok"}));
}

#[tokio::test]
async fn malformed_json_returns_bad_request() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, _body) = send(&state, rpc_request("{not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wrong_content_type_returns_unsupported_media_type() {
    let (state, _cancel) = app(Config::default());
    let request = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"id":"1","method":"server.info"}"#))
        .unwrap();

    let (status, _headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn oversized_body_returns_payload_too_large() {
    let (state, _cancel) = app(Config::default());
    let body = vec![b' '; http::RPC_BODY_LIMIT + 1];

    let (status, _headers, _body) = send(&state, rpc_request(body)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn invalid_envelope_returns_invalid_request_with_http_ok() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"6","method":"server.info","unknown":true}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["id"], "6");
    assert_eq!(value["error"]["code"], "invalidRequest");
}

#[tokio::test]
async fn unknown_method_returns_an_rpc_error_with_http_ok() {
    let (state, _cancel) = app(Config::default());

    let (status, headers, body) = send(
        &state,
        rpc_request(r#"{"id":"1","method":"unknown.method"}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let value = body_json(&body);
    assert_eq!(value["id"], "1");
    assert_eq!(value["error"]["code"], "unknownMethod");
}

#[tokio::test]
async fn server_info_reports_version_and_start_time() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"2","method":"server.info"}"#)).await;

    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["id"], "2");
    assert_eq!(value["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["result"]["protocolVersion"], "1.0");
    assert!(value["result"]["startedAt"].is_string());
}

#[tokio::test]
async fn invalid_params_return_an_rpc_error() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"3","method":"services.status","params":{}}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["error"]["code"], "invalidParams");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"3","method":"messages.list","params":{"order":2}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["error"]["code"], "invalidParams");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid message order code 2")
    );
}

#[tokio::test]
async fn missing_service_errors_include_the_reference() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"4","method":"services.send","params":{"service":{"alias":"MISSING"},"payload":"AQ=="}}"#,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["error"]["code"], "serviceNotFound");
    assert_eq!(value["error"]["details"]["alias"], "MISSING");
}

#[tokio::test]
async fn services_list_starts_empty() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"5","method":"services.list"}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["result"], json!({"services": []}));
}

#[tokio::test]
async fn message_list_returns_newest_first_with_next_bounds() {
    let (state, _cancel) = app(Config::default());
    let key = ServiceKey {
        protocol: "zwave".to_string(),
        transport: "serial".to_string(),
        entry: "COM3".to_string(),
    };
    let first = state.store.register(&key, &[1], MessageState::Incoming);
    let second = state.store.register(&key, &[2], MessageState::Outgoing);
    let third = state.store.register(&key, &[3], MessageState::Incoming);

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"20","method":"messages.list","params":{"order":0,"limit":2}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["result"]["storeTotal"], 3);
    assert_eq!(value["result"]["messages"][0]["id"], third.id.to_string());
    assert_eq!(
        value["result"]["messages"][0]["state"],
        serde_json::json!(0)
    );
    assert_eq!(value["result"]["messages"][1]["id"], second.id.to_string());
    assert_eq!(
        value["result"]["messages"][1]["state"],
        serde_json::json!(1)
    );
    let until = value["result"]["next"]["until"]
        .as_str()
        .expect("next until")
        .to_string();
    assert!(value["result"]["next"]["from"].is_null());

    let (status, _headers, body) = send(
        &state,
        rpc_request(Body::from(format!(
            r#"{{"id":"21","method":"messages.list","params":{{"order":0,"limit":2,"until":"{until}"}}}}"#
        ))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["result"]["messages"][0]["id"], first.id.to_string());
    assert!(value["result"]["next"].is_null());
}

#[tokio::test]
async fn message_list_filters_payload_and_metadata() {
    let (state, _cancel) = app(Config::default());
    let key = ServiceKey {
        protocol: "zwave".to_string(),
        transport: "serial".to_string(),
        entry: "COM3".to_string(),
    };
    let incoming = state
        .store
        .register(&key, &[0x01, 0x0a, 0x00], MessageState::Incoming);
    state
        .store
        .register(&key, &[0x02, 0x03], MessageState::OutgoingPending);

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"25","method":"messages.list","params":{
                "directions":[0],
                "protocols":["zwave"],
                "transports":["serial"],
                "payload":{"offset":0,"bytes":"AQoA"}
            }}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["result"]["storeTotal"], 2);
    assert_eq!(value["result"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["result"]["messages"][0]["id"],
        incoming.id.to_string()
    );

    let (status, _headers, body) = send(
        &state,
        rpc_request(Body::from(format!(
            r#"{{"id":"26","method":"messages.list","params":{{"ids":["{}","00000000-0000-0000-0000-000000000000"]}}}}"#,
            incoming.id
        ))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body_json(&body);
    assert_eq!(value["result"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["result"]["messages"][0]["id"],
        incoming.id.to_string()
    );
}

#[tokio::test]
async fn removed_snapshot_methods_are_unknown() {
    let (state, _cancel) = app(Config::default());
    for method in ["messages.snapshot", "messages.window", "messages.release"] {
        let (status, _headers, body) = send(
            &state,
            rpc_request(Body::from(format!(
                r#"{{"id":"30","method":"{method}","params":{{}}}}"#
            ))),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body_json(&body)["error"]["code"], "unknownMethod");
    }
}

#[tokio::test]
async fn read_only_methods_return_expected_shapes() {
    let (state, _cancel) = app(Config::default());

    let cases = [
        (
            r#"{"id":"10","method":"protocols.list"}"#,
            json!("zwave"),
            json!("/result/protocols/0/id"),
        ),
        (
            r#"{"id":"11","method":"transports.list"}"#,
            json!("serial"),
            json!("/result/transports/0/id"),
        ),
        (
            r#"{"id":"15","method":"protocols.list"}"#,
            json!(["serial"]),
            json!("/result/protocols/0/transports"),
        ),
        (
            r#"{"id":"12","method":"protocols.info","params":{"protocol":"zwave"}}"#,
            json!("zwave"),
            json!("/result/protocol/id"),
        ),
        (
            r#"{"id":"13","method":"config.get"}"#,
            json!(8180),
            json!("/result/httpServer/port"),
        ),
    ];

    for (request, expected, pointer) in cases {
        let (status, _headers, body) = send(&state, rpc_request(request)).await;
        assert_eq!(status, StatusCode::OK);
        let value = body_json(&body);
        assert_eq!(value.pointer(pointer.as_str().unwrap()).unwrap(), &expected);
    }

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"14","method":"messages.list"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_json(&body)["result"],
        json!({"messages": [], "storeTotal": 0})
    );

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"15","method":"discovery.get"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "discoveryNotFound");

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"16","method":"messages.get","params":{"id":"3f1d2c9a-8f7e-4a5b-9c0d-1e2f3a4b5c6d"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "messageNotFound");
}

#[tokio::test]
async fn serves_static_assets_spa_fallback_and_gzip() {
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "<html>index</html>").unwrap();
    std::fs::write(site.path().join("app.js"), "x".repeat(4096)).unwrap();

    let mut config = Config::default();
    config.http_server.static_assets.dir = site.path().to_path_buf();
    let (state, _cancel) = app(config);

    let request = Request::builder()
        .uri("/app.js")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 4096);

    let request = Request::builder()
        .uri("/some/frontend/route")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"<html>index</html>");

    let request = Request::builder()
        .uri("/app.js")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_ENCODING).unwrap(), "gzip");
}

#[tokio::test]
async fn json_assets_ignore_stale_precompressed_sidecars() {
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "<html>index</html>").unwrap();
    std::fs::write(site.path().join("zwave.json"), br#"{"name":"fresh"}"#).unwrap();
    // Simulate sidecars left over from an older build. tower-http would pick
    // these by Accept-Encoding even though the raw JSON is newer.
    std::fs::write(site.path().join("zwave.json.br"), b"stale-brotli").unwrap();
    std::fs::write(site.path().join("zwave.json.gz"), b"stale-gzip").unwrap();

    let mut config = Config::default();
    config.http_server.static_assets.dir = site.path().to_path_buf();
    config.http_server.static_assets.precompressed = true;
    let (state, _cancel) = app(config);

    let request = Request::builder()
        .uri("/zwave.json")
        .header(header::ACCEPT_ENCODING, "br")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(body, br#"{"name":"fresh"}"#);
}

#[tokio::test]
async fn static_assets_revalidate_by_modification_time() {
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "<html>index</html>").unwrap();
    std::fs::write(site.path().join("app.wasm"), b"\0asm\x01\0\0\0").unwrap();

    let mut config = Config::default();
    config.http_server.static_assets.dir = site.path().to_path_buf();
    let (state, _cancel) = app(config);

    let request = Request::builder()
        .uri("/app.wasm")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/wasm"
    );
    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=0, must-revalidate"
    );
    assert_eq!(
        headers
            .get(header::HeaderName::from_static(
                "cross-origin-opener-policy"
            ))
            .unwrap(),
        "same-origin"
    );
    assert_eq!(
        headers
            .get(header::HeaderName::from_static(
                "cross-origin-embedder-policy"
            ))
            .unwrap(),
        "require-corp"
    );
    let last_modified = headers.get(header::LAST_MODIFIED).unwrap().clone();

    let request = Request::builder()
        .uri("/app.wasm")
        .header(header::IF_MODIFIED_SINCE, last_modified)
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(headers.get(header::LAST_MODIFIED).is_some());
    assert!(body.is_empty());

    let request = Request::builder()
        .uri("/missing.wasm")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let request = Request::builder()
        .uri("/some/frontend/route")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"<html>index</html>");
}

#[tokio::test]
async fn rejects_requests_with_oversized_headers() {
    let mut config = Config::default();
    config.http_server.max_header_bytes = 256;
    let (state, _cancel) = app(config);

    let request = Request::builder()
        .uri("/api/health")
        .header("x-large", "a".repeat(512))
        .body(Body::empty())
        .unwrap();
    let (status, _headers, _body) = send(&state, request).await;

    assert_eq!(status, StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
}

#[tokio::test]
async fn returns_server_shutting_down_for_new_requests() {
    let (state, _cancel) = app(Config::default());
    state.shutting_down.store(true, Ordering::SeqCst);

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"1","method":"server.info"}"#)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "serverShuttingDown");
}

#[tokio::test]
async fn service_mutations_are_written_to_the_configuration_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let (state, _cancel) = app_with_config_path(Config::default(), path.clone());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"services.add","params":{"alias":"ZW","protocol":"zwave","transport":"serial","entry":"COM99"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.services.len(), 1);
    assert_eq!(saved.services[0].alias.as_deref(), Some("ZW"));
    assert_eq!(saved.services[0].entry, "COM99");

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"2","method":"config.get"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_json(&body)["result"]["services"][0]["alias"],
        json!("ZW")
    );

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"3","method":"services.alias","params":{"service":{"alias":"ZW"},"alias":"NEW"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.services[0].alias.as_deref(), Some("NEW"));

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"4","method":"services.remove","params":{"service":{"alias":"NEW"}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let saved = hodeco::config::load(&path).unwrap();
    assert!(saved.services.is_empty());
}

#[tokio::test]
async fn exposes_pair_specific_parameter_definitions() {
    let (state, _cancel) = app(Config::default());
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"protocols.params","params":{"protocol":"zwave","transport":"serial"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let response = body_json(&body);
    let params = response["result"]["params"]
        .as_array()
        .expect("parameter definitions");
    let data_bits = params
        .iter()
        .find(|definition| definition["name"] == "dataBits")
        .expect("dataBits definition");
    assert_eq!(data_bits["const"], false);
    assert!(
        params
            .iter()
            .any(|definition| definition["name"] == "baudRate")
    );

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"2","method":"protocols.params","params":{"protocol":"missing","transport":"serial"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "unknownProtocol");
}

#[tokio::test]
async fn updates_configuration_without_restarting_runtime_services() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let (state, _cancel) = app_with_config_path(Config::default(), path.clone());

    let mut next = Config::default();
    next.log.file = "updated.log".to_string();
    next.http_server.port = 9999;
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            json!({
                "id": "1",
                "method": "config.update",
                "params": {"config": next.clone()}
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert_eq!(body["result"]["config"]["httpServer"]["port"], 9999);
    assert_eq!(
        body["result"]["restartRequired"],
        json!(["log", "httpServer"])
    );

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.http_server.port, 9999);
    assert_eq!(saved.log.file, "updated.log");

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"2","method":"config.get"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["result"]["httpServer"]["port"], 9999);

    let mut invalid = next;
    invalid.http_server.port = 0;
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            json!({
                "id": "3",
                "method": "config.update",
                "params": {"config": invalid}
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "configInvalid");
    assert_eq!(hodeco::config::load(&path).unwrap().http_server.port, 9999);
}

#[tokio::test]
async fn config_update_preserves_configured_services() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let (state, _cancel) = app_with_config_path(Config::default(), path.clone());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"services.add","params":{"protocol":"zwave","transport":"serial","entry":"COM98"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let mut next = Config::default();
    next.log.file = "updated.log".to_string();
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            json!({
                "id": "2",
                "method": "config.update",
                "params": {"config": next}
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    let services = body["result"]["config"]["services"]
        .as_array()
        .expect("services array");
    assert_eq!(services.len(), 1);
    assert_eq!(services[0]["entry"], "COM98");
    // The service was added through services.add, so it is already live and
    // must not be reported as restart-required.
    assert_eq!(body["result"]["restartRequired"], json!(["log"]));

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.log.file, "updated.log");
    assert_eq!(saved.services.len(), 1);
    assert_eq!(saved.services[0].entry, "COM98");
}

#[tokio::test]
async fn updates_service_parameter_overrides_and_writes_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let (state, _cancel) = app_with_config_path(Config::default(), path.clone());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"services.add","params":{"alias":"ZW","protocol":"zwave","transport":"serial","entry":"COM98","params":{"baudRate":57600}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"2","method":"services.params","params":{"service":{"alias":"ZW"},"params":{"baudRate":9600}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert_eq!(body["result"]["service"]["params"]["baudRate"], 9600);

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.services[0].params.get("baudRate"), Some(&json!(9600)));

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"3","method":"services.params","params":{"service":{"alias":"ZW"},"params":{}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert!(body["result"]["service"]["params"].is_null());

    let saved = hodeco::config::load(&path).unwrap();
    assert!(saved.services[0].params.is_empty());
}

#[tokio::test]
async fn rolls_back_service_add_when_config_write_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _cancel) = app_with_config_path(Config::default(), dir.path().to_path_buf());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"services.add","params":{"alias":"ZW","protocol":"zwave","transport":"serial","entry":"COM99"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "configWriteFailed");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"2","method":"services.list"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["result"], json!({"services": []}));
}

#[tokio::test]
async fn serves_http_and_websocket_connections() {
    let port = free_port();
    let mut config = Config::default();
    config.http_server.port = port;
    let (state, _cancel) = app(config);
    let cancel = CancellationToken::new();
    let server = tokio::spawn(hodeco::http::serve(state, cancel.clone()));
    let address = format!("127.0.0.1:{port}");

    wait_for_listener(&address).await;

    let (mut socket, _) = connect_async(format!("ws://{address}/api/ws"))
        .await
        .expect("websocket connects");
    socket
        .send(WsMessage::Text(
            json!({"id":"1","method":"server.info"}).to_string().into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["id"], "1");
    assert_eq!(response["result"]["version"], env!("CARGO_PKG_VERSION"));

    let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
    stream
        .write_all(
            format!("GET /api/health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"status\":\"ok\""), "{response}");

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shutdown timed out")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn rejects_websocket_connections_over_the_limit() {
    let port = free_port();
    let mut config = Config::default();
    config.http_server.port = port;
    config.http_server.max_connections = 1;
    let (state, _cancel) = app(config);
    let cancel = CancellationToken::new();
    let server = tokio::spawn(hodeco::http::serve(state, cancel.clone()));
    let address = format!("127.0.0.1:{port}");

    wait_for_listener(&address).await;

    let (socket, _) = connect_async(format!("ws://{address}/api/ws"))
        .await
        .expect("first websocket connects");

    let mut second = tokio::spawn(connect_async(format!("ws://{address}/api/ws")));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut second)
            .await
            .is_err(),
        "second websocket must wait for a free connection slot"
    );

    drop(socket);
    let (second_socket, _) = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("second websocket did not connect after the first closed")
        .unwrap()
        .expect("second websocket connects");
    drop(second_socket);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shutdown timed out")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn enforces_the_header_read_timeout() {
    let port = free_port();
    let mut config = Config::default();
    config.http_server.port = port;
    config.http_server.read_header_timeout = HumanDuration::from_std(Duration::from_millis(100));
    let (address, cancel, server) = start_http_server(config);
    wait_for_listener(&address).await;

    let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
    stream
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: ")
        .await
        .unwrap();

    let started = Instant::now();
    let mut buffer = [0_u8; 256];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await;
    assert!(
        result.is_ok(),
        "server did not enforce readHeaderTimeout: {result:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(50));

    drop(stream);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shutdown timed out")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn enforces_the_idle_timeout_on_keep_alive_connections() {
    let port = free_port();
    let mut config = Config::default();
    config.http_server.port = port;
    config.http_server.idle_timeout = HumanDuration::from_std(Duration::from_millis(150));
    let (address, cancel, server) = start_http_server(config);
    wait_for_listener(&address).await;

    let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/health HTTP/1.1\r\nHost: {address}\r\nConnection: keep-alive\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut response = Vec::new();
    let mut buffer = [0_u8; 256];
    loop {
        let count = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
            .await
            .expect("timed out waiting for the health response")
            .unwrap();
        assert!(count > 0, "connection closed before the response");
        response.extend_from_slice(&buffer[..count]);
        if response
            .windows(b"\"status\":\"ok\"".len())
            .any(|window| window == b"\"status\":\"ok\"")
        {
            break;
        }
    }

    let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await;
    assert!(
        closed.is_ok(),
        "server did not enforce idleTimeout on a keep-alive connection"
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shutdown timed out")
        .unwrap()
        .unwrap();
}

fn start_http_server(
    config: Config,
) -> (
    String,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), hodeco::app::AppError>>,
) {
    let port = config.http_server.port;
    let (state, _cancel) = app(config);
    let cancel = CancellationToken::new();
    let server = tokio::spawn(hodeco::http::serve(state, cancel.clone()));
    (format!("127.0.0.1:{port}"), cancel, server)
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn wait_for_listener(address: &str) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server did not start listening on {address}");
}

#[tokio::test]
async fn websocket_closes_when_a_resync_is_requested() {
    let (state, _cancel) = app(Config::default());
    let resync = state.resync.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, http::router(state)).await.unwrap();
    });

    let (mut socket, _) = connect_async(format!("ws://{address}/api/ws"))
        .await
        .expect("websocket connects");
    socket
        .send(WsMessage::Text(
            json!({"id":"1","method":"server.info"}).to_string().into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["id"], "1");

    resync.send(()).unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("timed out waiting for resync close")
        .expect("websocket stream ended")
        .expect("websocket receive failed");
    match message {
        WsMessage::Close(Some(frame)) => assert_eq!(
            frame.code,
            tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Again
        ),
        other => panic!("expected resync close frame, got {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn websocket_notifications_follow_topic_subscriptions() {
    let (state, _cancel) = app(Config::default());
    let store = state.store.clone();
    let events = state.events.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, http::router(state)).await.unwrap();
    });

    let (mut socket, _) = connect_async(format!("ws://{address}/api/ws"))
        .await
        .expect("websocket connects");

    socket
        .send(WsMessage::Text(
            json!({
                "id": "1",
                "method": "events.subscribe",
                "params": {"topics": ["messages"]}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["id"], "1");
    assert_eq!(response["result"], json!({}));

    let key = ServiceKey {
        protocol: "zwave".to_string(),
        transport: "serial".to_string(),
        entry: "COM3".to_string(),
    };
    let message = store.register(&key, &[1, 2, 3], MessageState::Incoming);
    events.send(Event::MessageNew(message.clone())).unwrap();

    let notification = next_json(&mut socket).await;
    assert_eq!(notification["method"], "events.messageNew");
    assert_eq!(
        notification["params"]["message"]["id"],
        message.id.to_string()
    );

    socket
        .send(WsMessage::Text(
            json!({
                "id": "2",
                "method": "events.unsubscribe",
                "params": {"topics": ["messages"]}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["id"], "2");

    let message = store.register(&key, &[4, 5, 6], MessageState::Incoming);
    events.send(Event::MessageNew(message)).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), next_json(&mut socket))
            .await
            .is_err(),
        "unsubscribed connection must not receive notifications"
    );

    server.abort();
}

async fn next_json<S>(socket: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("websocket receive timed out")
            .expect("websocket stream ended")
            .expect("websocket receive failed");
        match message {
            WsMessage::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
            other => panic!("unexpected websocket message: {other:?}"),
        }
    }
}
