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
use hodeco::automation::history::HistoryStore;
use hodeco::automation::runtime::TaskRuntime;
use hodeco::automation::store::{AutomationSnapshot, AutomationStore};
use hodeco::config::{CatalogPaths, ConfigStore, ServicesStore};
use hodeco::events::{self, Event};
use hodeco::http;
use hodeco::messages::MessageStore;
use hodeco::services::ServiceError;
use hodeco::services::discovery::{DiscoveryBackend, DiscoveryRegistry};
use hodeco::services::registry::ServiceRegistry;
use hodeco_contract::config::{Config, ServicesFile, TaskLogConfig};
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
    let services_path = hodeco::config::catalog_path(&path, &config).services;
    app_with_state(ConfigStore::new(Some(path), config), Some(services_path))
}

fn app_with_config_store(config_store: ConfigStore) -> (AppState, CancellationToken) {
    app_with_state(config_store, None)
}

fn app_with_state(
    config_store: ConfigStore,
    services_path: Option<PathBuf>,
) -> (AppState, CancellationToken) {
    let cancel = CancellationToken::new();
    let store = Arc::new(MessageStore::new(1024 * 1024));
    let registry = Arc::new(ServiceRegistry::with_defaults(store.clone(), &cancel));
    let discovery = Arc::new(DiscoveryRegistry::new(Arc::new(EmptyDiscovery), &cancel));
    let (events, _) = events::channel();
    let (resync, _) = tokio::sync::broadcast::channel(1);
    let config = config_store.snapshot();
    let ws_limit = config.http_server.max_connections.max(1) as usize;
    let services_store = Arc::new(ServicesStore::new(services_path, Vec::new()));
    let catalog_dir = std::env::temp_dir().join(format!("hodeco-http-{}", uuid::Uuid::new_v4()));
    let automation = Arc::new(AutomationStore::new(
        AutomationSnapshot {
            tasks: Default::default(),
            schedules: Default::default(),
            commands: Default::default(),
            location: None,
            timezone: None,
        },
        CatalogPaths {
            services: catalog_dir.join("services.yaml"),
            tasks: catalog_dir.join("tasks.yaml"),
            schedules: catalog_dir.join("schedules.yaml"),
            commands: catalog_dir.join("commands.yaml"),
        },
    ));
    let history_dir = catalog_dir.join("history");
    let history = Arc::new(HistoryStore::new(TaskLogConfig::default(), history_dir));
    let runtime = Arc::new(TaskRuntime::new(config.task_engine.max_concurrent_runs));
    let state = AppState {
        config: Arc::new(config),
        config_store: Arc::new(config_store),
        services_store,
        automation,
        runtime,
        history,
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

    let services_path = dir.path().join("services.yaml");
    let saved: ServicesFile = hodeco::config::read_yaml(&services_path).unwrap();
    assert_eq!(saved.services.len(), 1);
    assert_eq!(saved.services[0].alias.as_deref(), Some("ZW"));
    assert_eq!(saved.services[0].entry, "COM99");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"2","method":"services.list"}"#),
    )
    .await;
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

    let saved: ServicesFile = hodeco::config::read_yaml(&services_path).unwrap();
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

    let saved: ServicesFile = hodeco::config::read_yaml(&services_path).unwrap();
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
    // Services are managed through services.* and are not part of config.get.
    assert!(body["result"]["config"]["services"].is_null());
    // The service was added through services.add, so it is already live and
    // must not be reported as restart-required.
    assert_eq!(body["result"]["restartRequired"], json!(["log"]));

    let saved = hodeco::config::load(&path).unwrap();
    assert_eq!(saved.log.file, "updated.log");
    let services: ServicesFile =
        hodeco::config::read_yaml(&dir.path().join("services.yaml")).unwrap();
    assert_eq!(services.services.len(), 1);
    assert_eq!(services.services[0].entry, "COM98");
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

    let saved: ServicesFile = hodeco::config::read_yaml(&dir.path().join("services.yaml")).unwrap();
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

    let saved: ServicesFile = hodeco::config::read_yaml(&dir.path().join("services.yaml")).unwrap();
    assert!(saved.services[0].params.is_empty());
}

#[tokio::test]
async fn rolls_back_service_add_when_config_write_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.files.services = PathBuf::from("blocked.yaml");
    std::fs::create_dir(dir.path().join("blocked.yaml")).unwrap();
    let (state, _cancel) = app_with_config_path(config, dir.path().join("config.yaml"));

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

#[tokio::test]
async fn task_crud_fire_and_history_round_trip() {
    let (state, _cancel) = app(Config::default());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"tasks.add","params":{"task":{"id":"test-task","name":"Test task"}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert_eq!(body["result"]["task"]["task"]["id"], "test-task");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"2","method":"tasks.fire","params":{"taskId":"test-task"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    let run_id = body["result"]["runId"].as_str().unwrap().to_string();

    let mut runs = Vec::new();
    for _ in 0..40 {
        let (status, _headers, body) = send(
            &state,
            rpc_request(
                r#"{"id":"3","method":"tasks.history","params":{"taskIds":["test-task"]}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body_json(&body);
        runs = body["result"]["runs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !runs.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["trigger"]["type"], "manual");
    assert_eq!(runs[0]["outcome"], "succeeded");

    let (status, _headers, body) = send(
        &state,
        rpc_request(format!(
            r#"{{"id":"4","method":"tasks.get","params":{{"taskId":"test-task","runId":"{run_id}"}}}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert_eq!(body["result"]["run"]["summary"]["id"], run_id);

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"5","method":"tasks.update","params":{"taskId":"test-task","task":{"id":"test-task","name":"Test task","enabled":false}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"6","method":"tasks.fire","params":{"taskId":"test-task"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["error"]["code"], "taskDisabled");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"7","method":"tasks.remove","params":{"taskId":"test-task"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"8","method":"tasks.list"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["result"]["tasks"], json!([]));
}

#[tokio::test]
async fn http_hook_starts_a_task_and_checks_the_secret() {
    let (state, _cancel) = app(Config::default());
    let task = hodeco_contract::domain::TaskDefinition {
        id: "hook-task".to_string(),
        name: "Hook task".to_string(),
        triggers: vec![hodeco_contract::domain::TaskTrigger::Http(
            hodeco_contract::domain::HttpTrigger {
                id: "hook".to_string(),
                methods: vec![hodeco_contract::domain::HttpMethod::Post],
                secret: Some("s3cret".to_string()),
            },
        )],
        ..Default::default()
    };
    let mut snapshot = state.automation.snapshot();
    snapshot.tasks.insert(task.id.clone(), task);
    state.automation.replace_snapshot(snapshot);

    let request = Request::builder()
        .method("POST")
        .uri("/api/hooks/hook-task")
        .body(Body::from("hello"))
        .unwrap();
    let (status, _headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let request = Request::builder()
        .method("POST")
        .uri("/api/hooks/hook-task")
        .header("x-hodeco-secret", "s3cret")
        .body(Body::from("hello"))
        .unwrap();
    let (status, _headers, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body_json(&body)["runId"].is_string());

    let request = Request::builder()
        .method("GET")
        .uri("/api/hooks/hook-task")
        .header("x-hodeco-secret", "s3cret")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let request = Request::builder()
        .uri("/api/hooks/missing")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, _body) = send(&state, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn schedule_crud_and_preview_round_trip() {
    let (state, _cancel) = app(Config::default());
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"schedules.add","params":{"schedule":{"id":"morning","name":"Morning","timezone":"UTC","on":{"at":"09:00"}}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"2","method":"schedules.list"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert_eq!(body["result"]["schedules"][0]["schedule"]["id"], "morning");
    assert!(body["result"]["schedules"][0]["next"].is_object());

    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"3","method":"schedules.preview","params":{"schedule":{"id":"morning","name":"Morning","timezone":"UTC","on":{"at":"09:00"}},"limit":3}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert!(body["error"].is_null(), "{body}");
    assert_eq!(body["result"]["occurrences"].as_array().unwrap().len(), 3);

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"4","method":"schedules.remove","params":{"scheduleId":"morning"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());
}

#[tokio::test]
async fn task_command_action_captures_output_and_history() {
    let (state, _cancel) = app(Config::default());
    #[cfg(windows)]
    let (executable, args) = ("cmd", vec!["/C", "echo hello"]);
    #[cfg(not(windows))]
    let (executable, args) = ("sh", vec!["-c", "echo hello"]);

    let command = json!({
        "command": {
            "id": "echo",
            "name": "Echo",
            "executable": executable,
            "args": args,
            "timeout": "5s"
        }
    });
    let (status, _headers, body) = send(
        &state,
        rpc_request(json!({"id": "1", "method": "commands.add", "params": command}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let task = json!({
        "task": {
            "id": "echo-task",
            "name": "Echo task",
            "actions": [{"id": "run", "command": "echo", "output": "capture"}]
        }
    });
    let (status, _headers, body) = send(
        &state,
        rpc_request(json!({"id": "2", "method": "tasks.add", "params": task}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"3","method":"tasks.fire","params":{"taskId":"echo-task"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let mut run = Value::Null;
    for _ in 0..80 {
        let (status, _headers, body) = send(
            &state,
            rpc_request(
                r#"{"id":"4","method":"tasks.history","params":{"taskIds":["echo-task"]}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body_json(&body);
        if let Some(first) = body["result"]["runs"]
            .as_array()
            .and_then(|runs| runs.first())
        {
            let run_id = first["id"].as_str().unwrap();
            let (status, _headers, body) = send(
                &state,
                rpc_request(
                    json!({
                        "id": "5",
                        "method": "tasks.get",
                        "params": {"taskId": "echo-task", "runId": run_id}
                    })
                    .to_string(),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let body = body_json(&body);
            if body["error"].is_null() {
                run = body["result"]["run"].clone();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(run["summary"]["outcome"], "succeeded", "{run}");
    assert_eq!(run["actions"][0]["status"], "succeeded", "{run}");
    assert!(
        run["actions"][0]["command"]["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("hello"),
        "{run}"
    );
}

#[tokio::test]
async fn next_rule_starts_a_child_task_and_records_the_chain() {
    let (state, _cancel) = app(Config::default());
    for (id, body) in [
        (
            "1",
            json!({
                "id": "child",
                "method": "tasks.add",
                "params": {"task": {"id": "child-task", "name": "Child"}}
            }),
        ),
        (
            "2",
            json!({
                "id": "parent",
                "method": "tasks.add",
                "params": {"task": {
                    "id": "parent-task",
                    "name": "Parent",
                    "next": [{"when": "always", "tasks": "child-task"}]
                }}
            }),
        ),
    ] {
        let (status, _headers, response) = send(&state, rpc_request(body.to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body_json(&response)["error"].is_null(),
            "{id}: {response:?}"
        );
    }

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"3","method":"tasks.fire","params":{"taskId":"parent-task"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    let mut parent = Value::Null;
    for _ in 0..80 {
        let (status, _headers, body) = send(
            &state,
            rpc_request(
                r#"{"id":"4","method":"tasks.history","params":{"taskIds":["parent-task"]}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body_json(&body);
        if let Some(first) = body["result"]["runs"]
            .as_array()
            .and_then(|runs| runs.first())
        {
            let run_id = first["id"].as_str().unwrap();
            let (status, _headers, body) = send(
                &state,
                rpc_request(
                    json!({
                        "id": "5",
                        "method": "tasks.get",
                        "params": {"taskId": "parent-task", "runId": run_id}
                    })
                    .to_string(),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let body = body_json(&body);
            if body["error"].is_null() {
                parent = body["result"]["run"].clone();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(parent["summary"]["nextCount"], 1, "{parent}");
    assert_eq!(parent["nextTasks"][0]["taskId"], "child-task", "{parent}");
    assert_eq!(parent["nextTasks"][0]["rule"], "always", "{parent}");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"6","method":"tasks.history","params":{"taskIds":["child-task"]}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body_json(&body);
    assert_eq!(body["result"]["runs"][0]["trigger"]["type"], "task");
}

#[tokio::test]
async fn scheduler_fires_a_second_interval_schedule() {
    let (state, _cancel) = app(Config::default());
    let from = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let schedule = json!({
        "schedule": {
            "id": "tick",
            "name": "Tick",
            "timezone": "UTC",
            "from": from,
            "on": {"every": "1s"}
        }
    });
    let (status, _headers, body) = send(
        &state,
        rpc_request(json!({"id":"1","method":"schedules.add","params":schedule}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let task = json!({
        "task": {
            "id": "tick-task",
            "name": "Tick task",
            "triggers": [{"id": "tick-trigger", "type": "schedule", "schedule": "tick"}]
        }
    });
    let (status, _headers, body) = send(
        &state,
        rpc_request(json!({"id":"2","method":"tasks.add","params":task}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let scheduler_cancel = CancellationToken::new();
    let scheduler = tokio::spawn(hodeco::automation::scheduler::run(
        state.clone(),
        scheduler_cancel.clone(),
    ));

    let mut seen = false;
    for _ in 0..120 {
        let (status, _headers, body) = send(
            &state,
            rpc_request(
                r#"{"id":"3","method":"tasks.history","params":{"taskIds":["tick-task"]}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body_json(&body);
        if body["result"]["runs"]
            .as_array()
            .is_some_and(|runs| !runs.is_empty())
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    scheduler_cancel.cancel();
    scheduler.abort();
    assert!(seen, "scheduler did not fire the interval schedule");
}

#[tokio::test]
async fn task_history_can_be_disabled_with_max_rows_zero() {
    let (state, _cancel) = app(Config::default());
    let (status, _headers, body) = send(
        &state,
        rpc_request(
            r#"{"id":"1","method":"tasks.add","params":{"task":{"id":"no-history","name":"No history","history":{"maxRows":0,"maxSize":"0 B"}}}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null(), "{body:?}");

    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"2","method":"tasks.fire","params":{"taskId":"no-history"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_json(&body)["error"].is_null());

    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, _headers, body) = send(
        &state,
        rpc_request(r#"{"id":"3","method":"tasks.history","params":{"taskIds":["no-history"]}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&body)["result"]["runs"], json!([]));

    let (status, _headers, body) =
        send(&state, rpc_request(r#"{"id":"4","method":"tasks.list"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body_json(&body)["result"]["tasks"][0]["lastRun"].is_object(),
        "runtime last run should still be visible"
    );
}
