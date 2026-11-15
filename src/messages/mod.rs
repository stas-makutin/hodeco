pub mod persist;
pub mod store;

use hodeco_contract::domain::{Message, MessageState, ServiceKey};
pub use persist::{PersistError, PersistedLog};
pub use store::{MessageFilter, MessagePage, MessageStore, StoreEvent};
use uuid::Uuid;

/// Records messages from services without coupling services to the store.
pub trait MessageRecorder: Send + Sync {
    fn register(&self, service: &ServiceKey, payload: &[u8], state: MessageState) -> Message;
    fn update_state(&self, id: Uuid, state: MessageState) -> Option<Message>;
}
