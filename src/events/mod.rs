pub mod types;

pub use types::{Event, Topic, UnknownTopic};

use tokio::sync::broadcast;

pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

pub fn channel() -> (broadcast::Sender<Event>, broadcast::Receiver<Event>) {
    broadcast::channel(EVENT_CHANNEL_CAPACITY)
}
