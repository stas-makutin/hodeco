//! WebSocket transport plus a local demo transport.

use crate::demo::DemoServer;
use ewebsock::{Options, WsEvent, WsMessage};
use std::collections::VecDeque;
use std::sync::Arc;
use web_time::Instant;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    Opened,
    Text(String),
    Error(String),
    Closed,
}

#[allow(clippy::large_enum_variant)] // Exactly one transport lives for the session.
pub enum Transport {
    Live(LiveTransport),
    Demo(DemoTransport),
}

impl Transport {
    pub fn poll(&mut self, now: Instant) -> Vec<TransportEvent> {
        match self {
            Self::Live(transport) => transport.poll(),
            Self::Demo(transport) => transport.poll(now),
        }
    }

    pub fn send(&mut self, text: String) -> Result<(), String> {
        match self {
            Self::Live(transport) => transport.send(text),
            Self::Demo(transport) => transport.send(text),
        }
    }
}

pub struct LiveTransport {
    sender: ewebsock::WsSender,
    receiver: ewebsock::WsReceiver,
}

impl LiveTransport {
    pub fn connect(endpoint: &str, wake: Wake) -> Result<Self, String> {
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(endpoint, Options::default(), move || wake())?;
        Ok(Self { sender, receiver })
    }

    fn send(&mut self, text: String) -> Result<(), String> {
        self.sender.send(WsMessage::Text(text));
        Ok(())
    }

    fn poll(&mut self) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.receiver.try_recv() {
            match event {
                WsEvent::Opened => events.push(TransportEvent::Opened),
                WsEvent::Message(WsMessage::Text(text)) => {
                    events.push(TransportEvent::Text(text));
                }
                WsEvent::Message(WsMessage::Binary(bytes)) => {
                    events.push(TransportEvent::Text(
                        String::from_utf8_lossy(&bytes).into_owned(),
                    ));
                }
                WsEvent::Message(WsMessage::Unknown(_)) => {}
                WsEvent::Message(WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
                WsEvent::Error(error) => events.push(TransportEvent::Error(error)),
                WsEvent::Closed => events.push(TransportEvent::Closed),
            }
        }
        events
    }
}

pub struct DemoTransport {
    server: DemoServer,
    queue: VecDeque<String>,
    opened: bool,
    next_message_at: Instant,
}

impl DemoTransport {
    pub fn new(now: Instant) -> Self {
        Self {
            server: DemoServer::new(),
            queue: VecDeque::new(),
            opened: false,
            next_message_at: now + std::time::Duration::from_secs(2),
        }
    }

    fn send(&mut self, text: String) -> Result<(), String> {
        for response in self.server.handle(&text) {
            self.queue.push_back(response);
        }
        Ok(())
    }

    fn poll(&mut self, now: Instant) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        if !self.opened {
            self.opened = true;
            events.push(TransportEvent::Opened);
        }
        while let Some(text) = self.queue.pop_front() {
            events.push(TransportEvent::Text(text));
        }
        if now >= self.next_message_at {
            for text in self.server.tick() {
                events.push(TransportEvent::Text(text));
            }
            self.next_message_at = now + std::time::Duration::from_secs(2);
        }
        events
    }
}
