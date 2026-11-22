use crate::app::AppState;
use crate::events::Event;
use crate::http::rpc::{self, RawRequest, Session};
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::stream::SplitSink;
use futures_util::stream::SplitStream;
use hodeco_contract::api::notifications::{
    DiscoveryNotification, MessageDroppedNotification, MessageNotification,
    ServiceStateChangedNotification,
};
use hodeco_contract::api::{ApiError, ErrorCode, Notification, Response};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

pub const OUTGOING_QUEUE_CAPACITY: usize = 256;
pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

const WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn handle(socket: WebSocket, state: AppState) {
    let (sink, stream) = socket.split();
    let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_QUEUE_CAPACITY);
    let (close_tx, close_rx) = watch::channel::<Option<CloseFrame>>(None);
    let last_pong = Arc::new(AtomicU64::new(now_millis()));
    let cancel = CancellationToken::new();

    let mut writer = tokio::spawn(writer_loop(
        sink,
        outgoing_rx,
        close_rx,
        last_pong.clone(),
        cancel.clone(),
    ));

    read_loop(stream, &state, &outgoing, &close_tx, &last_pong, &cancel).await;
    drop(outgoing);

    if tokio::time::timeout(WRITER_SHUTDOWN_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
        warn!("websocket writer shutdown timed out");
        writer.abort();
    }
}

async fn read_loop(
    mut stream: SplitStream<WebSocket>,
    state: &AppState,
    outgoing: &mpsc::Sender<String>,
    close_tx: &watch::Sender<Option<CloseFrame>>,
    last_pong: &AtomicU64,
    cancel: &CancellationToken,
) {
    let mut events = state.events.subscribe();
    let mut resync = state.resync.subscribe();
    let mut session = Session::default();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = state.cancel.cancelled() => {
                request_close(close_tx, 1001, "server shutting down");
                return;
            }
            resync_event = resync.recv() => {
                match resync_event {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        warn!("event resync requested; closing websocket");
                        request_close(close_tx, 1013, "event stream resync required");
                        return;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
            message = stream.next() => {
                let Some(message) = message else {
                    return;
                };
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        debug!(error = %error, "websocket connection closed");
                        return;
                    }
                };
                touch(last_pong);

                let text = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => {
                            if !queue_text(
                                outgoing,
                                close_tx,
                                invalid_request_text("request body is not valid UTF-8"),
                            ) {
                                return;
                            }
                            continue;
                        }
                    },
                    Message::Close(_) => return,
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                let (response, restart) = request_text(state, &mut session, &text).await;
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        error!(error = %error, "failed to encode websocket response");
                        request_close(close_tx, 1011, "response encoding failed");
                        return;
                    }
                };
                if !queue_text(outgoing, close_tx, response) {
                    return;
                }
                if restart {
                    state.request_restart();
                    return;
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if !session.is_subscribed(event.topic()) {
                            continue;
                        }
                        let notification = notification(event);
                        match serde_json::to_string(&notification) {
                            Ok(text) => {
                                if !queue_text(outgoing, close_tx, text) {
                                    return;
                                }
                            }
                            Err(error) => {
                                error!(error = %error, "failed to encode event notification");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "websocket event stream lagged; closing for resync");
                        request_close(close_tx, 1013, "event stream lagged");
                        return;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

async fn request_text(
    state: &AppState,
    session: &mut Session,
    text: &str,
) -> (Result<String, serde_json::Error>, bool) {
    let (response, restart) = match serde_json::from_str::<RawRequest>(text) {
        Ok(request) => rpc::execute(state, session, request).await,
        Err(error) => (
            Response::error(
                "",
                ApiError::new(
                    ErrorCode::InvalidRequest,
                    format!("invalid request: {error}"),
                ),
            ),
            false,
        ),
    };
    (serde_json::to_string(&response), restart)
}

fn invalid_request_text(message: &str) -> String {
    serde_json::to_string(&Response::error(
        "",
        ApiError::new(ErrorCode::InvalidRequest, message),
    ))
    .expect("error responses always serialize")
}

async fn writer_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut outgoing: mpsc::Receiver<String>,
    mut close: watch::Receiver<Option<CloseFrame>>,
    last_pong: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    let mut ping = interval_at(TokioInstant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            changed = close.changed() => {
                if changed.is_err() {
                    break;
                }
                let frame = close.borrow_and_update().clone();
                if let Some(frame) = frame {
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    break;
                }
            }
            message = outgoing.recv() => {
                match message {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: 1000,
                                reason: "connection closed".into(),
                            })))
                            .await;
                        break;
                    }
                }
            }
            _ = ping.tick() => {
                let idle = now_millis().saturating_sub(last_pong.load(Ordering::Relaxed))
                    >= IDLE_TIMEOUT.as_millis() as u64;
                if idle {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code: 1001,
                            reason: "idle timeout".into(),
                        })))
                        .await;
                    break;
                }
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }

    cancel.cancel();
}

fn queue_text(
    outgoing: &mpsc::Sender<String>,
    close_tx: &watch::Sender<Option<CloseFrame>>,
    text: String,
) -> bool {
    match outgoing.try_send(text) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("closing slow websocket consumer");
            request_close(close_tx, 1013, "slow consumer");
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn request_close(close_tx: &watch::Sender<Option<CloseFrame>>, code: u16, reason: &'static str) {
    let _ = close_tx.send_if_modified(|current| {
        if current.is_none() {
            *current = Some(CloseFrame {
                code,
                reason: reason.into(),
            });
            true
        } else {
            false
        }
    });
}

fn touch(last_pong: &AtomicU64) {
    last_pong.store(now_millis(), Ordering::Relaxed);
}

fn now_millis() -> u64 {
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    CLOCK
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn notification(event: Event) -> Notification {
    match event {
        Event::DiscoveryStarted(discovery) => {
            Notification::DiscoveryStarted(DiscoveryNotification { discovery })
        }
        Event::DiscoveryFinished(discovery) => {
            Notification::DiscoveryFinished(DiscoveryNotification { discovery })
        }
        Event::MessageNew(message) => Notification::MessageNew(MessageNotification { message }),
        Event::MessageDropped { message_ids } => {
            Notification::MessageDropped(MessageDroppedNotification { message_ids })
        }
        Event::MessageStateChanged(message) => {
            Notification::MessageStateChanged(MessageNotification { message })
        }
        Event::ServiceStateChanged { service, status } => {
            Notification::ServiceStateChanged(ServiceStateChangedNotification { service, status })
        }
        Event::TaskRunStarted(notification) => Notification::TaskRunStarted(notification),
        Event::TaskRunFinished(notification) => Notification::TaskRunFinished(notification),
        Event::TaskRunDropped { run_ids } => Notification::TaskRunDropped(
            hodeco_contract::api::notifications::TaskRunDroppedNotification { run_ids },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflowing_outgoing_queue_requests_a_slow_consumer_close() {
        let (outgoing, _receiver) = mpsc::channel(1);
        outgoing.try_send("first".to_string()).unwrap();
        let (close_tx, close_rx) = watch::channel(None);

        assert!(!queue_text(&outgoing, &close_tx, "second".to_string()));

        let close = close_rx.borrow().clone().expect("close frame");
        assert_eq!(close.code, 1013);
        assert_eq!(close.reason, "slow consumer");
    }
}
