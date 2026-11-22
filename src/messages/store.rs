use crate::messages::MessageRecorder;
use chrono::{DateTime, TimeDelta, Utc};
use hodeco_contract::api::payloads::MessageTimeRange;
use hodeco_contract::domain::{
    Message, MessageDirection, MessageOrder, MessageState, PayloadExpr, ServiceKey,
};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::sync::broadcast;
use uuid::Uuid;
pub const MESSAGE_ENTRY_OVERHEAD: u64 = 29;
pub const MINIMUM_STORE_SIZE: u64 = 8 * 1024;
const EVENT_CHANNEL_CAPACITY: usize = 4096;

pub fn message_entry_length(payload_length: usize) -> u64 {
    MESSAGE_ENTRY_OVERHEAD + payload_length as u64
}

pub fn service_entry_length(service: &ServiceKey) -> u64 {
    4 + service.protocol.len() as u64 + service.transport.len() as u64 + service.entry.len() as u64
}

#[derive(Debug, Clone)]
pub enum StoreEvent {
    New { key: ServiceKey, message: Message },
    StateChanged { key: ServiceKey, message: Message },
    Dropped { message_ids: Vec<Uuid> },
}

#[derive(Debug, Clone, Default)]
pub struct MessageFilter {
    pub states: Vec<MessageState>,
    pub directions: Vec<MessageDirection>,
    pub protocols: Vec<String>,
    pub transports: Vec<String>,
    pub services: Vec<ServiceKey>,
    /// Candidate IDs. Empty means no ID restriction.
    pub ids: Vec<Uuid>,
    pub payload: Option<PayloadExpr>,
}

impl MessageFilter {
    fn matches(&self, key: &ServiceKey, message: &Message) -> bool {
        if !self.states.is_empty() && !self.states.contains(&message.state) {
            return false;
        }
        if !self.directions.is_empty() && !self.directions.contains(&message.state.direction()) {
            return false;
        }
        if !self.protocols.is_empty() && !self.protocols.contains(&key.protocol) {
            return false;
        }
        if !self.transports.is_empty() && !self.transports.contains(&key.transport) {
            return false;
        }
        if !self.services.is_empty() && !self.services.contains(key) {
            return false;
        }
        if let Some(payload) = &self.payload
            && !payload.matches(message.payload.as_slice())
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    pub next: Option<MessageTimeRange>,
    pub store_total: u64,
}

#[derive(Debug)]
struct StoredMessage {
    key: ServiceKey,
    message: Message,
}

#[derive(Debug, Default)]
struct StoreInner {
    entries: VecDeque<StoredMessage>,
    index: HashMap<Uuid, u64>,
    service_counts: HashMap<ServiceKey, u64>,
    next_seq: u64,
    base_seq: u64,
    bytes: u64,
    last_time: Option<DateTime<Utc>>,
}

impl StoreInner {
    fn position(&self, id: &Uuid) -> Option<usize> {
        let seq = *self.index.get(id)?;
        usize::try_from(seq.saturating_sub(self.base_seq)).ok()
    }

    fn next_timestamp(&mut self, now: DateTime<Utc>) -> DateTime<Utc> {
        let time = match self.last_time {
            Some(last) if now <= last => last
                .checked_add_signed(TimeDelta::nanoseconds(1))
                .unwrap_or(now),
            _ => now,
        };
        self.last_time = Some(time);
        time
    }
}

pub struct MessageStore {
    inner: RwLock<StoreInner>,
    max_bytes: u64,
    events: broadcast::Sender<StoreEvent>,
}

impl MessageStore {
    pub fn new(max_bytes: u64) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: RwLock::new(StoreInner::default()),
            max_bytes: max_bytes.max(MINIMUM_STORE_SIZE),
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    pub fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> u64 {
        self.inner.read().bytes
    }

    pub fn register(&self, service: &ServiceKey, payload: &[u8], state: MessageState) -> Message {
        let mut inner = self.inner.write();

        let entry_length = message_entry_length(payload.len());
        let mut dropped = Vec::new();

        loop {
            // Recompute the service overhead every time: evicting a message can
            // remove the last entry for the incoming service, and the new
            // message then has to re-add that overhead.
            let service_length = if inner.service_counts.contains_key(service) {
                0
            } else {
                service_entry_length(service)
            };
            if inner.bytes + entry_length + service_length <= self.max_bytes {
                break;
            }
            let Some(front) = inner.entries.pop_front() else {
                break;
            };
            inner.index.remove(&front.message.id);
            inner.bytes -= message_entry_length(front.message.payload.len());

            let count = inner
                .service_counts
                .get_mut(&front.key)
                .expect("service count must exist for stored message");
            *count -= 1;
            if *count == 0 {
                inner.service_counts.remove(&front.key);
                inner.bytes -= service_entry_length(&front.key);
            }

            inner.base_seq += 1;
            dropped.push(front.message.id);
        }

        let time = inner.next_timestamp(Utc::now());
        let message = Message::from_key(Uuid::new_v4(), time, service, state, payload.to_vec());

        let seq = inner.next_seq;
        inner.next_seq += 1;
        let new_service = !inner.service_counts.contains_key(service);
        if new_service {
            inner.bytes += service_entry_length(service);
        }
        inner.bytes += entry_length;
        inner.entries.push_back(StoredMessage {
            key: service.clone(),
            message: message.clone(),
        });
        inner.index.insert(message.id, seq);
        if new_service {
            inner.service_counts.insert(service.clone(), 1);
        } else {
            *inner
                .service_counts
                .get_mut(service)
                .expect("service count was checked above") += 1;
        }

        drop(inner);

        if !dropped.is_empty() {
            let _ = self.events.send(StoreEvent::Dropped {
                message_ids: dropped,
            });
        }
        let _ = self.events.send(StoreEvent::New {
            key: service.clone(),
            message: message.clone(),
        });
        message
    }

    pub fn update_state(&self, id: Uuid, state: MessageState) -> Option<Message> {
        let (key, message) = {
            let mut inner = self.inner.write();
            let position = inner.position(&id)?;
            let entry = inner.entries.get_mut(position)?;
            entry.message.state = state;
            (entry.key.clone(), entry.message.clone())
        };

        let _ = self.events.send(StoreEvent::StateChanged {
            key,
            message: message.clone(),
        });
        Some(message)
    }

    pub fn get(&self, id: Uuid) -> Option<Message> {
        let inner = self.inner.read();
        let position = inner.position(&id)?;
        inner
            .entries
            .get(position)
            .map(|entry| entry.message.clone())
    }

    /// Lists messages in timestamp order.
    ///
    /// `from` is inclusive and `until` is exclusive. The returned `next` bound
    /// pair continues from the last row in the same paging direction.
    pub fn list(
        &self,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        order: MessageOrder,
        limit: usize,
        filter: &MessageFilter,
    ) -> MessagePage {
        let inner = self.inner.read();
        let length = inner.entries.len();
        let store_total = length as u64;
        let limit = limit.max(1);

        let empty = MessagePage {
            messages: Vec::new(),
            next: None,
            store_total,
        };
        if from.is_some_and(|from| until.is_some_and(|until| from >= until)) {
            return empty;
        }

        let start = from.map_or(0, |from| {
            inner
                .entries
                .partition_point(|entry| entry.message.time < from)
        });
        let end = until.map_or(length, |until| {
            inner
                .entries
                .partition_point(|entry| entry.message.time < until)
        });
        if start >= end {
            return empty;
        }

        let positions: Box<dyn Iterator<Item = usize> + '_> = if filter.ids.is_empty() {
            if order == MessageOrder::OldestFirst {
                Box::new(start..end)
            } else {
                Box::new((start..end).rev())
            }
        } else {
            let mut seen = HashSet::new();
            let mut candidates = Vec::new();
            for id in &filter.ids {
                if !seen.insert(*id) {
                    continue;
                }
                let Some(position) = inner.position(id) else {
                    continue;
                };
                if position < start || position >= end {
                    continue;
                }
                candidates.push(position);
            }
            candidates.sort_unstable();
            if order == MessageOrder::NewestFirst {
                candidates.reverse();
            }
            Box::new(candidates.into_iter())
        };

        let mut messages = Vec::with_capacity(limit.min(end - start));
        let mut has_more = false;
        for position in positions {
            let entry = &inner.entries[position];
            if !filter.matches(&entry.key, &entry.message) {
                continue;
            }
            if messages.len() == limit {
                has_more = true;
                break;
            }
            messages.push(entry.message.clone());
        }

        let next = if has_more {
            messages.last().map(|message| match order {
                MessageOrder::NewestFirst => MessageTimeRange {
                    from,
                    until: Some(message.time),
                },
                MessageOrder::OldestFirst => MessageTimeRange {
                    from: message
                        .time
                        .checked_add_signed(TimeDelta::nanoseconds(1))
                        .or(from),
                    until,
                },
            })
        } else {
            None
        };

        MessagePage {
            messages,
            next,
            store_total,
        }
    }

    pub(crate) fn snapshot(&self) -> (Vec<(ServiceKey, Message)>, u64) {
        let inner = self.inner.read();
        let entries = inner
            .entries
            .iter()
            .map(|entry| (entry.key.clone(), entry.message.clone()))
            .collect();
        (entries, inner.bytes)
    }

    pub(crate) fn replace_all(&self, entries: Vec<(ServiceKey, Message)>, bytes: u64) {
        let mut inner = self.inner.write();
        inner.entries.clear();
        inner.index.clear();
        inner.service_counts.clear();
        inner.next_seq = 0;
        inner.base_seq = 0;
        inner.bytes = bytes;
        inner.last_time = None;

        for (key, mut message) in entries {
            if let Some(last) = inner.last_time
                && message.time <= last
            {
                message.time = last
                    .checked_add_signed(TimeDelta::nanoseconds(1))
                    .unwrap_or(last);
            }
            inner.last_time = Some(message.time);
            let seq = inner.next_seq;
            inner.next_seq += 1;
            inner.index.insert(message.id, seq);
            *inner.service_counts.entry(key.clone()).or_insert(0) += 1;
            inner.entries.push_back(StoredMessage { key, message });
        }
    }
}

impl MessageRecorder for MessageStore {
    fn register(&self, service: &ServiceKey, payload: &[u8], state: MessageState) -> Message {
        MessageStore::register(self, service, payload, state)
    }

    fn update_state(&self, id: Uuid, state: MessageState) -> Option<Message> {
        MessageStore::update_state(self, id, state)
    }
}

impl std::fmt::Debug for MessageStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageStore")
            .field("len", &self.len())
            .field("bytes", &self.bytes())
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use hodeco_contract::domain::PayloadOffset;

    fn key(entry: &str) -> ServiceKey {
        ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: entry.to_string(),
        }
    }

    fn list(store: &MessageStore, filter: &MessageFilter) -> MessagePage {
        store.list(None, None, MessageOrder::OldestFirst, 100, filter)
    }

    #[test]
    fn stores_messages_in_insertion_order_with_increasing_timestamps() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");

        let first = store.register(&key, &[1], MessageState::Incoming);
        let second = store.register(&key, &[2], MessageState::Outgoing);
        let third = store.register(&key, &[3], MessageState::Incoming);

        assert!(first.time < second.time);
        assert!(second.time < third.time);
        let page = list(&store, &MessageFilter::default());
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![first.id, second.id, third.id]
        );
        assert!(page.next.is_none());
        assert_eq!(page.store_total, 3);
        assert!(matches!(
            page.messages[0].service,
            hodeco_contract::domain::ServiceRef::Key { .. }
        ));
    }

    #[test]
    fn updates_state_without_reordering() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");

        let first = store.register(&key, &[1], MessageState::OutgoingPending);
        let second = store.register(&key, &[2], MessageState::Incoming);
        store.update_state(first.id, MessageState::Outgoing);

        let page = list(&store, &MessageFilter::default());
        assert_eq!(page.messages[0].id, first.id);
        assert_eq!(page.messages[0].state, MessageState::Outgoing);
        assert_eq!(page.messages[1].id, second.id);
        assert_eq!(store.get(second.id).unwrap().state, MessageState::Incoming);
    }

    #[test]
    fn filters_metadata_and_payload() {
        let store = MessageStore::new(1024 * 1024);
        let first_key = key("COM3");
        let second_key = key("COM4");

        store.register(&first_key, &[1, 2, 3], MessageState::Incoming);
        store.register(&second_key, &[4, 5, 6], MessageState::Outgoing);
        store.register(&second_key, &[1, 2, 3], MessageState::Outgoing);

        let filter = MessageFilter {
            states: vec![MessageState::Outgoing],
            services: vec![second_key.clone()],
            payload: Some(PayloadExpr::and(vec![
                PayloadExpr::leaf(PayloadOffset::At(0), vec![1]),
                PayloadExpr::leaf(PayloadOffset::At(2), vec![3]),
            ])),
            ..Default::default()
        };
        let page = list(&store, &filter);
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].payload.as_slice(), &[1, 2, 3]);
        assert_eq!(page.messages[0].state, MessageState::Outgoing);
    }

    #[test]
    fn filters_by_direction_protocol_transport_and_payload_bytes() {
        let store = MessageStore::new(1024 * 1024);
        let zwave = key("COM4");
        let http = ServiceKey {
            protocol: "http".to_string(),
            transport: "tcp".to_string(),
            entry: "127.0.0.1:8080".to_string(),
        };
        store.register(&zwave, b"Hello", MessageState::Incoming);
        let outgoing = store.register(&http, b"world", MessageState::OutgoingPending);

        let filter = MessageFilter {
            directions: vec![MessageDirection::Outgoing],
            protocols: vec!["http".to_string()],
            transports: vec!["tcp".to_string()],
            payload: Some(PayloadExpr::anywhere(b"orl".to_vec())),
            ..Default::default()
        };
        assert_eq!(list(&store, &filter).messages[0].id, outgoing.id);

        store.update_state(outgoing.id, MessageState::Outgoing);
        assert_eq!(
            store.get(outgoing.id).unwrap().state,
            MessageState::Outgoing
        );
    }

    #[test]
    fn filters_by_ids_without_duplicates() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");
        let first = store.register(&key, &[1], MessageState::Incoming);
        let second = store.register(&key, &[2], MessageState::Incoming);
        let third = store.register(&key, &[3], MessageState::Incoming);

        let filter = MessageFilter {
            ids: vec![third.id, first.id, third.id, Uuid::new_v4()],
            ..Default::default()
        };
        let newest = store.list(None, None, MessageOrder::NewestFirst, 10, &filter);
        assert_eq!(
            newest
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![third.id, first.id]
        );
        let oldest = list(&store, &filter);
        assert_eq!(
            oldest
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![first.id, third.id]
        );
        assert!(
            !oldest
                .messages
                .iter()
                .any(|message| message.id == second.id)
        );
    }

    #[test]
    fn paginates_newest_first_with_time_bounds() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");
        let messages: Vec<Message> = (0..5)
            .map(|value| store.register(&key, &[value], MessageState::Incoming))
            .collect();

        let first = store.list(
            None,
            None,
            MessageOrder::NewestFirst,
            2,
            &MessageFilter::default(),
        );
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![messages[4].id, messages[3].id]
        );
        let next = first.next.expect("next bounds");
        assert_eq!(next.from, None);
        assert_eq!(next.until, Some(messages[3].time));

        let second = store.list(
            next.from,
            next.until,
            MessageOrder::NewestFirst,
            2,
            &MessageFilter::default(),
        );
        assert_eq!(
            second
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![messages[2].id, messages[1].id]
        );
        let next = second.next.expect("next bounds");
        let third = store.list(
            next.from,
            next.until,
            MessageOrder::NewestFirst,
            2,
            &MessageFilter::default(),
        );
        assert_eq!(third.messages[0].id, messages[0].id);
        assert!(third.next.is_none());
    }

    #[test]
    fn paginates_oldest_first_with_inclusive_from() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");
        let messages: Vec<Message> = (0..4)
            .map(|value| store.register(&key, &[value], MessageState::Incoming))
            .collect();

        let first = store.list(
            None,
            None,
            MessageOrder::OldestFirst,
            2,
            &MessageFilter::default(),
        );
        assert_eq!(first.messages[0].id, messages[0].id);
        assert_eq!(first.messages[1].id, messages[1].id);
        let next = first.next.expect("next bounds");
        let expected = messages[1]
            .time
            .checked_add_signed(TimeDelta::nanoseconds(1))
            .unwrap();
        assert_eq!(next.from, Some(expected));
        assert_eq!(next.until, None);

        let second = store.list(
            next.from,
            next.until,
            MessageOrder::OldestFirst,
            2,
            &MessageFilter::default(),
        );
        assert_eq!(second.messages[0].id, messages[2].id);
        assert_eq!(second.messages[1].id, messages[3].id);
        assert!(second.next.is_none());
    }

    #[test]
    fn time_bounds_are_inclusive_and_exclusive() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");
        let first = store.register(&key, &[1], MessageState::Incoming);
        let second = store.register(&key, &[2], MessageState::Incoming);

        let page = store.list(
            Some(first.time),
            Some(second.time),
            MessageOrder::OldestFirst,
            10,
            &MessageFilter::default(),
        );
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![first.id]
        );

        let empty = store.list(
            Some(second.time),
            Some(second.time),
            MessageOrder::OldestFirst,
            10,
            &MessageFilter::default(),
        );
        assert!(empty.messages.is_empty());
    }

    #[test]
    fn normalizes_persisted_timestamps_in_insertion_order() {
        let store = MessageStore::new(1024 * 1024);
        let key = key("COM3");
        let base = Utc::now();
        let entries = vec![
            (
                key.clone(),
                Message::from_key(Uuid::new_v4(), base, &key, MessageState::Incoming, vec![1]),
            ),
            (
                key.clone(),
                Message::from_key(Uuid::new_v4(), base, &key, MessageState::Incoming, vec![2]),
            ),
            (
                key.clone(),
                Message::from_key(
                    Uuid::new_v4(),
                    base - TimeDelta::seconds(1),
                    &key,
                    MessageState::OutgoingPending,
                    vec![3],
                ),
            ),
        ];
        store.replace_all(entries, 0);
        let page = list(&store, &MessageFilter::default());
        assert!(page.messages[0].time < page.messages[1].time);
        assert!(page.messages[1].time < page.messages[2].time);

        let next = store.register(&key, &[4], MessageState::Incoming);
        assert!(next.time > page.messages[2].time);
    }

    #[test]
    fn eviction_recounts_service_overhead_when_the_last_entry_is_replaced() {
        let store = MessageStore::new(MINIMUM_STORE_SIZE);
        let first = key("COM3");
        let second = key("COM4");
        let payload = vec![0_u8; 3000];

        store.register(&first, &payload, MessageState::Incoming);
        store.register(&second, &payload, MessageState::Incoming);
        // This evicts the only `first` message and must add its service
        // overhead again for the incoming message.
        store.register(&first, &payload, MessageState::Incoming);

        let (entries, bytes) = store.snapshot();
        let mut expected = 0;
        let mut services = HashSet::new();
        for (key, message) in &entries {
            expected += message_entry_length(message.payload.len());
            if services.insert(key.clone()) {
                expected += service_entry_length(key);
            }
        }
        assert_eq!(bytes, expected);
        assert!(bytes <= MINIMUM_STORE_SIZE);
    }

    #[test]
    fn evicts_oldest_messages_by_size() {
        let store = MessageStore::new(MINIMUM_STORE_SIZE);
        let key = key("COM3");
        let payload = vec![0_u8; 2048];

        let mut ids = Vec::new();
        for _ in 0..8 {
            ids.push(store.register(&key, &payload, MessageState::Incoming).id);
        }

        assert!(store.bytes() <= MINIMUM_STORE_SIZE);
        assert!(store.len() < ids.len());
        assert!(store.get(ids[0]).is_none());
        assert!(store.get(*ids.last().unwrap()).is_some());
    }

    #[test]
    fn reports_evicted_messages() {
        let store = MessageStore::new(MINIMUM_STORE_SIZE);
        let key = key("COM3");
        let mut events = store.subscribe();
        let payload = vec![0_u8; 4096];

        store.register(&key, &payload, MessageState::Incoming);
        store.register(&key, &payload, MessageState::Incoming);

        let mut dropped = Vec::new();
        let mut new_count = 0;
        while let Ok(event) = events.try_recv() {
            match event {
                StoreEvent::Dropped { message_ids } => {
                    dropped.extend(message_ids);
                }
                StoreEvent::New { .. } => new_count += 1,
                StoreEvent::StateChanged { .. } => {}
            }
        }
        assert_eq!(dropped.len(), 1);
        assert_eq!(new_count, 2);
    }
}
