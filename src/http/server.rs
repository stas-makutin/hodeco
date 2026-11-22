use crate::app::{AppError, AppState};
use crate::automation::engine::{self, FireError};
use crate::http::{assets, rpc, ws};
use axum::Extension;
use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hodeco_contract::api::{ApiError, ErrorCode, Response as ApiResponse};
use hodeco_contract::domain::{HttpMethod, HttpRunTrigger, RunChain, RunTrigger};
use hodeco_contract::utils::Base64Bytes;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use tower_http::compression::CompressionLayer;
use tower_http::timeout::TimeoutBody;
use tracing::{debug, error, info, warn};

pub const RPC_BODY_LIMIT: usize = 512 * 1024;

const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);
const HTTP1_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct HttpLimits {
    max_header_bytes: usize,
    read_timeout: Duration,
    write_timeout: Duration,
}

#[derive(Clone)]
struct RemoteAddr(SocketAddr);

pub fn router(state: AppState) -> Router {
    let limits = HttpLimits {
        max_header_bytes: state.config.http_server.max_header_bytes as usize,
        read_timeout: state.config.http_server.read_timeout.as_std(),
        write_timeout: state.config.http_server.write_timeout.as_std(),
    };

    let api = Router::new()
        .route("/rpc", post(rpc_handler))
        .route("/ws", get(websocket_handler))
        .route("/health", get(health))
        .route("/hooks/{task}", get(hook_handler).post(hook_handler))
        .layer(DefaultBodyLimit::max(RPC_BODY_LIMIT))
        .with_state(state.clone());

    Router::new()
        .nest("/api", api)
        .merge(assets::router(&state.config.http_server.static_assets))
        .layer(CompressionLayer::new().gzip(true))
        .layer(middleware::from_fn(move |request, next| {
            apply_http_limits(limits, request, next)
        }))
        .layer(middleware::from_fn(cross_origin_isolation))
}

async fn apply_http_limits(limits: HttpLimits, request: Request, next: Next) -> Response {
    if request_header_bytes(&request) > limits.max_header_bytes {
        return StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.into_response();
    }

    let (parts, body) = request.into_parts();
    let body = Body::new(TimeoutBody::new(limits.read_timeout, body));
    let response = next.run(Request::from_parts(parts, body)).await;

    with_response_timeout(response, limits.write_timeout)
}

fn with_response_timeout(response: Response, timeout: Duration) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::new(TimeoutBody::new(timeout, body)))
}

fn request_header_bytes(request: &Request) -> usize {
    let mut bytes = request.method().as_str().len()
        + request
            .uri()
            .path_and_query()
            .map_or(0, |value| value.as_str().len())
        + 12;

    for (name, value) in request.headers() {
        bytes = bytes.saturating_add(name.as_str().len() + value.as_bytes().len() + 4);
    }

    bytes
}

async fn cross_origin_isolation(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("cross-origin-embedder-policy"),
        HeaderValue::from_static("require-corp"),
    );
    response
}

pub async fn serve(state: AppState, cancel: CancellationToken) -> Result<(), AppError> {
    let config = state.config.http_server.clone();
    let shutting_down = state.shutting_down.clone();
    let address = bind_address(&config.bind, config.port);
    let listener = TcpListener::bind(&address)
        .await
        .map_err(|error| format!("failed to bind http server to '{address}': {error}"))?;
    let local = listener.local_addr()?;
    info!(address = %local, "http server listening");

    let app = router(state);
    let max_connections = if config.max_connections == 0 {
        Semaphore::MAX_PERMITS
    } else {
        config.max_connections as usize
    };
    let semaphore = Arc::new(Semaphore::new(max_connections));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                shutting_down.store(true, Ordering::SeqCst);
                break;
            }
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = result {
                    debug!(error = %error, "http connection task failed");
                }
            }
            permit = semaphore.clone().acquire_owned() => {
                let permit = match permit {
                    Ok(permit) => permit,
                    Err(_) => break,
                };

                let accepted = tokio::select! {
                    _ = cancel.cancelled() => {
                        shutting_down.store(true, Ordering::SeqCst);
                        break;
                    }
                    result = listener.accept() => result,
                };
                let (stream, remote) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        warn!(error = %error, "http connection accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                if let Err(error) = stream.set_nodelay(true) {
                    debug!(error = %error, "unable to enable TCP_NODELAY");
                }

                let activity = ConnectionActivity::new();
                let request_activity = activity.clone();
                let permit_slot = PermitSlot::new(permit);
                let request_permit = permit_slot.clone();
                let remote_addr = RemoteAddr(remote);
                let service = app.clone().map_request(move |mut request: Request<Incoming>| {
                    request_activity.touch();
                    request.extensions_mut().insert(request_permit.clone());
                    request.extensions_mut().insert(remote_addr.clone());
                    request.map(Body::new)
                });
                let hyper_service = TowerToHyperService::new(service);
                let header_read_timeout = config.read_header_timeout.as_std();
                let idle_timeout = config.idle_timeout.as_std();
                let max_header_bytes = config.max_header_bytes as usize;
                let mut shutdown = shutdown_rx.clone();

                connections.spawn(async move {
                    let _connection_permit = permit_slot;
                    let io = TokioIo::new(stream);
                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    builder
                        .http1()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Some(header_read_timeout))
                        .max_buf_size(max_header_bytes.max(HTTP1_BUFFER_SIZE));
                    builder
                        .http2()
                        .timer(TokioTimer::new())
                        .max_header_list_size(u32::try_from(max_header_bytes).unwrap_or(u32::MAX));

                    let connection = builder.serve_connection_with_upgrades(io, hyper_service);
                    tokio::pin!(connection);

                    let mut idle_timer = tokio::time::interval(
                        idle_timeout
                            .min(Duration::from_secs(1))
                            .max(Duration::from_millis(50)),
                    );
                    idle_timer.set_missed_tick_behavior(
                        tokio::time::MissedTickBehavior::Delay,
                    );
                    let mut idle_shutdown = false;
                    let mut shutdown_requested = false;

                    loop {
                        tokio::select! {
                            result = connection.as_mut() => {
                                if let Err(error) = result {
                                    debug!(error = %error, "http connection failed");
                                }
                                break;
                            }
                            changed = shutdown.changed() => {
                                if !shutdown_requested {
                                    shutdown_requested = true;
                                    if changed.is_err() || *shutdown.borrow() {
                                        connection.as_mut().graceful_shutdown();
                                    }
                                }
                            }
                            _ = idle_timer.tick() => {
                                if !idle_shutdown && activity.elapsed() >= idle_timeout {
                                    idle_shutdown = true;
                                    connection.as_mut().graceful_shutdown();
                                }
                            }
                        }
                    }
                });
            }
        }
    }

    let _ = shutdown_tx.send(true);
    drain_connections(&mut connections, SHUTDOWN_GRACE_PERIOD).await;

    info!("http server stopped");
    Ok(())
}

async fn drain_connections(connections: &mut JoinSet<()>, grace: Duration) {
    let wait = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(grace, wait).await.is_err() {
        warn!("http server shutdown grace period elapsed; aborting connections");
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

/// Carries the accepted HTTP connection permit into a WebSocket upgrade so the
/// upgraded connection keeps counting against `maxConnections`.
#[derive(Clone)]
struct PermitSlot(Arc<tokio::sync::Mutex<Option<OwnedSemaphorePermit>>>);

impl PermitSlot {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self(Arc::new(tokio::sync::Mutex::new(Some(permit))))
    }

    async fn take(&self) -> Option<OwnedSemaphorePermit> {
        self.0.lock().await.take()
    }
}

#[derive(Clone)]
struct ConnectionActivity {
    inner: Arc<ConnectionActivityInner>,
}

struct ConnectionActivityInner {
    start: Instant,
    last_millis: AtomicU64,
}

impl ConnectionActivity {
    fn new() -> Self {
        Self {
            inner: Arc::new(ConnectionActivityInner {
                start: Instant::now(),
                last_millis: AtomicU64::new(0),
            }),
        }
    }

    fn touch(&self) {
        self.inner
            .last_millis
            .store(millis_since(self.inner.start), Ordering::Relaxed);
    }

    fn elapsed(&self) -> Duration {
        self.inner
            .start
            .elapsed()
            .saturating_sub(Duration::from_millis(
                self.inner.last_millis.load(Ordering::Relaxed),
            ))
    }
}

fn millis_since(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn rpc_handler(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !is_json_content_type(&headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "content type must be application/json",
        )
            .into_response();
    }

    // Parse the envelope directly on the success path. The body is only
    // reparsed to recover an `id` (or to distinguish malformed JSON) when the
    // request is rejected.
    let request = match serde_json::from_slice::<rpc::RawRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            let value: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => return (StatusCode::BAD_REQUEST, "invalid JSON").into_response(),
            };
            let id = value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            return api_response(&ApiResponse::error(
                id,
                ApiError::new(
                    ErrorCode::InvalidRequest,
                    format!("invalid request: {error}"),
                ),
            ));
        }
    };

    let (response, restart) = rpc::execute(&state, &mut rpc::Session::default(), request).await;
    let response = api_response(&response);

    if restart {
        state.request_restart();
    }

    response
}

async fn websocket_handler(
    State(state): State<AppState>,
    permit_slot: Option<Extension<PermitSlot>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if state.is_shutting_down() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    let permit = match permit_slot {
        Some(Extension(slot)) => slot.take().await,
        None => None,
    }
    .or_else(|| state.ws_limits.clone().try_acquire_owned().ok());
    let Some(permit) = permit else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    upgrade
        .max_message_size(RPC_BODY_LIMIT)
        .max_frame_size(RPC_BODY_LIMIT)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            ws::handle(socket, state).await;
        })
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn hook_handler(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    remote: Option<Extension<RemoteAddr>>,
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.is_shutting_down() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(task) = state.automation.task(&task_id) else {
        return hook_error(StatusCode::NOT_FOUND, "taskNotFound");
    };
    let Some(trigger) = task.http_trigger() else {
        return hook_error(StatusCode::NOT_FOUND, "hookNotFound");
    };
    let method = match method {
        axum::http::Method::GET => HttpMethod::Get,
        axum::http::Method::POST => HttpMethod::Post,
        _ => return hook_error(StatusCode::METHOD_NOT_ALLOWED, "methodNotAllowed"),
    };
    if !trigger.allows(method) {
        return hook_error(StatusCode::METHOD_NOT_ALLOWED, "methodNotAllowed");
    }
    if let Some(expected) = &trigger.secret {
        let provided = headers
            .get("x-hodeco-secret")
            .and_then(|value| value.to_str().ok());
        if !provided.is_some_and(|provided| secret_matches(expected, provided)) {
            return hook_error(StatusCode::UNAUTHORIZED, "invalidSecret");
        }
    }
    if body.len() > state.config.task_engine.max_trigger_payload_size.as_bytes() as usize {
        return hook_error(StatusCode::PAYLOAD_TOO_LARGE, "payloadTooLarge");
    }

    let mut metadata_headers = std::collections::BTreeMap::new();
    for name in ["user-agent", "content-type", "x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            metadata_headers.insert(name.to_string(), value.to_string());
        }
    }
    let trigger_record = RunTrigger::Http(HttpRunTrigger {
        id: trigger.id.clone(),
        method,
        path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        remote: remote.map(|Extension(address)| address.0.ip().to_string()),
        headers: metadata_headers,
        payload: (!body.is_empty()).then(|| Base64Bytes::from(body.as_ref())),
    });
    let chain = RunChain {
        chain_id: uuid::Uuid::new_v4(),
        parent_run_id: None,
        depth: 0,
    };
    match engine::fire(&state, &task, trigger_record, chain) {
        Ok(run_id) => (StatusCode::ACCEPTED, Json(json!({"runId": run_id}))).into_response(),
        Err(FireError::TaskDisabled) => hook_error(StatusCode::CONFLICT, "taskDisabled"),
        Err(FireError::TaskBusy) => hook_error(StatusCode::CONFLICT, "taskBusy"),
        Err(FireError::TaskLimitExceeded) => {
            hook_error(StatusCode::TOO_MANY_REQUESTS, "taskLimitExceeded")
        }
        Err(FireError::PayloadTooLarge) => {
            hook_error(StatusCode::PAYLOAD_TOO_LARGE, "payloadTooLarge")
        }
        Err(FireError::TaskNotFound | FireError::TriggerNotFound) => {
            hook_error(StatusCode::NOT_FOUND, "hookNotFound")
        }
    }
}

fn hook_error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}

fn secret_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(provided) {
        difference |= left ^ right;
    }
    difference == 0
}

fn json_response(body: Vec<u8>) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn bind_address(bind: &str, port: u16) -> String {
    if bind.contains(':') && !bind.starts_with('[') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    }
}

fn api_response(response: &ApiResponse) -> Response {
    match rpc::encode(response) {
        Ok(body) => json_response(body),
        Err(error) => {
            error!(error = %error, "failed to encode RPC response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn connection_drain_aborts_after_the_grace_period() {
        let mut connections = JoinSet::new();
        connections.spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let started = Instant::now();
        drain_connections(&mut connections, Duration::from_millis(20)).await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(connections.is_empty());
    }

    #[tokio::test]
    async fn response_body_timeout_aborts_stalled_bodies() {
        let body =
            Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
        let mut response = with_response_timeout(Response::new(body), Duration::from_millis(20));

        let frame = tokio::time::timeout(Duration::from_secs(1), response.body_mut().frame())
            .await
            .expect("timeout body did not resolve")
            .expect("timeout body ended without an error");

        assert!(frame.is_err());
    }

    #[test]
    fn accepts_json_content_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(is_json_content_type(&headers));

        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(!is_json_content_type(&headers));
    }

    #[test]
    fn formats_ipv4_and_ipv6_bind_addresses() {
        assert_eq!(bind_address("127.0.0.1", 8180), "127.0.0.1:8180");
        assert_eq!(bind_address("::1", 8180), "[::1]:8180");
    }

    #[test]
    fn encodes_api_responses_as_json() {
        let response = api_response(&ApiResponse::success("1", json!({})));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }
}
