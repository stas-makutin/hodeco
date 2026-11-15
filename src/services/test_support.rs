use super::serial::{Port, PortOpener, SerialSettings};
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub struct MockPortOpener {
    ports: Mutex<VecDeque<io::Result<Port>>>,
}

impl MockPortOpener {
    pub fn new(ports: Vec<io::Result<Port>>) -> Arc<Self> {
        Arc::new(Self {
            ports: Mutex::new(ports.into()),
        })
    }
}

impl PortOpener for MockPortOpener {
    fn open(
        &self,
        _entry: String,
        _settings: SerialSettings,
    ) -> Pin<Box<dyn Future<Output = io::Result<Port>> + Send + 'static>> {
        let port = self.ports.lock().unwrap().pop_front();
        Box::pin(async move {
            port.unwrap_or_else(|| {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no mock port available",
                ))
            })
        })
    }
}
