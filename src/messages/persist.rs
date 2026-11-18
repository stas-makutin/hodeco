//! HLOG v2 append log.
//!
//! The log is a shared container: magic, version, reserved bytes, and the last
//! compaction timestamp, followed by framed records. Message snapshots are
//! written by compaction; normal operation appends NEW, STATE, and DROPPED
//! records. HLOG v1 is not read or migrated.

use super::MessageStore;
use super::store::{StoreEvent, message_entry_length, service_entry_length};
use chrono::{TimeZone, Utc};
use hodeco_contract::domain::{Message, MessageState, ServiceKey};
use hodeco_contract::utils::FileMode;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MAGIC: [u8; 4] = *b"HLOG";
pub const VERSION: u8 = 2;
const RESERVED: [u8; 3] = [0; 3];
const HEADER_LEN: usize = 4 + 1 + 3 + 8;
const RECORD_OVERHEAD: usize = 1 + 4;
const RECORD_NEW: u8 = 0x01;
const RECORD_STATE: u8 = 0x02;
const RECORD_DROPPED: u8 = 0x03;

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("unable to access the message log: {0}")]
    Io(#[from] io::Error),
    #[error("message log path '{0}' does not name a file")]
    InvalidPath(PathBuf),
    #[error("the message log is corrupt: {0}")]
    Corrupt(String),
    #[error("the message log contains too many services or records")]
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedLog {
    pub entries: Vec<(ServiceKey, Message)>,
    pub bytes: u64,
    pub compacted_at: i64,
}

pub fn save(
    path: &Path,
    store: &MessageStore,
    file_mode: Option<FileMode>,
) -> Result<(), PersistError> {
    let (entries, _) = store.snapshot();
    let encoded = encode(&entries)?;
    write_atomic(path, &encoded, file_mode)
}

fn write_atomic(path: &Path, data: &[u8], file_mode: Option<FileMode>) -> Result<(), PersistError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| PersistError::InvalidPath(path.to_path_buf()))?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".{}.tmp", Uuid::new_v4()));
    let temporary = path.with_file_name(temporary_name);

    let result = (|| -> Result<(), io::Error> {
        let file = fs::File::create(&temporary)?;
        if let Some(mode) = file_mode {
            apply_mode(&file, mode)?;
        }
        let mut writer = BufWriter::new(file);
        writer.write_all(data)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(PersistError::Io)
}

#[cfg(unix)]
fn apply_mode(file: &fs::File, mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode.as_u32()))
}

#[cfg(not(unix))]
fn apply_mode(_file: &fs::File, _mode: FileMode) -> io::Result<()> {
    Ok(())
}

pub fn load(path: &Path) -> Result<Option<PersistedLog>, PersistError> {
    match fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PersistError::Io(error)),
        Ok(data) => decode(&data).map(Some),
    }
}

/// Writes one snapshot as NEW records.
pub fn encode(entries: &[(ServiceKey, Message)]) -> Result<Vec<u8>, PersistError> {
    let mut data = header(Utc::now().timestamp_nanos_opt().unwrap_or(0));
    for (key, message) in entries {
        let record = encode_new(key, message)?;
        push_record(&mut data, RECORD_NEW, &record);
    }
    Ok(data)
}

pub fn decode(data: &[u8]) -> Result<PersistedLog, PersistError> {
    if data.len() < HEADER_LEN {
        return Err(PersistError::Corrupt("header is truncated".to_string()));
    }
    if data[..4] != MAGIC {
        return Err(PersistError::Corrupt("invalid magic".to_string()));
    }
    if data[4] != VERSION {
        return Err(PersistError::Corrupt(if data[4] == 1 {
            "HLOG v1 is not supported; delete or archive the old message log".to_string()
        } else {
            format!("unsupported version {}", data[4])
        }));
    }
    let compacted_at = i64::from_le_bytes(data[8..16].try_into().unwrap());
    let mut entries: Vec<Option<(ServiceKey, Message)>> = Vec::new();
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    let mut offset = HEADER_LEN;

    while offset < data.len() {
        if offset + RECORD_OVERHEAD > data.len() {
            // A partial trailing record is ignored; a crash between the length
            // and the body must not make an otherwise usable log fail.
            break;
        }
        let record_type = data[offset];
        let length = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().unwrap()) as usize;
        offset += RECORD_OVERHEAD;
        if offset + length > data.len() {
            break;
        }
        let body = &data[offset..offset + length];
        offset += length;

        match record_type {
            RECORD_NEW => {
                let (key, message) = decode_new(body)?;
                if let Some(position) = index.get(&message.id).copied() {
                    entries[position] = Some((key, message));
                } else {
                    index.insert(message.id, entries.len());
                    entries.push(Some((key, message)));
                }
            }
            RECORD_STATE => {
                let mut cursor = Cursor::new(body);
                let mut uuid = [0_u8; 16];
                cursor.read_exact(&mut uuid).map_err(corrupt)?;
                let id = Uuid::from_bytes(uuid);
                let state = byte_to_state(read_u8(&mut cursor)?)?;
                if let Some(position) = index.get(&id).copied()
                    && let Some((_, message)) = &mut entries[position]
                {
                    message.state = state;
                }
            }
            RECORD_DROPPED => {
                let mut cursor = Cursor::new(body);
                let count = read_u16(&mut cursor)? as usize;
                for _ in 0..count {
                    let mut uuid = [0_u8; 16];
                    cursor.read_exact(&mut uuid).map_err(corrupt)?;
                    if let Some(position) = index.remove(&Uuid::from_bytes(uuid)) {
                        entries[position] = None;
                    }
                }
            }
            _ => {
                // Unknown record types are skipped so a newer writer can add
                // records without making this reader fail.
            }
        }
    }

    let entries: Vec<(ServiceKey, Message)> = entries.into_iter().flatten().collect();
    let mut bytes = 0_u64;
    let mut services = std::collections::BTreeSet::new();
    for (key, message) in &entries {
        if services.insert(key.clone()) {
            bytes += service_entry_length(key);
        }
        bytes += message_entry_length(message.payload.len());
    }
    Ok(PersistedLog {
        entries,
        bytes,
        compacted_at,
    })
}

fn header(compacted_at: i64) -> Vec<u8> {
    let mut data = Vec::with_capacity(HEADER_LEN);
    data.extend_from_slice(&MAGIC);
    data.push(VERSION);
    data.extend_from_slice(&RESERVED);
    data.extend_from_slice(&compacted_at.to_le_bytes());
    data
}

fn push_record(data: &mut Vec<u8>, record_type: u8, body: &[u8]) {
    data.push(record_type);
    data.extend_from_slice(&(body.len() as u32).to_le_bytes());
    data.extend_from_slice(body);
}

fn encode_new(key: &ServiceKey, message: &Message) -> Result<Vec<u8>, PersistError> {
    let mut record = Vec::new();
    write_string(&mut record, &key.protocol)?;
    write_string(&mut record, &key.transport)?;
    write_string(&mut record, &key.entry)?;
    record.extend_from_slice(message.id.as_bytes());
    let time = message
        .time
        .timestamp_nanos_opt()
        .ok_or_else(|| PersistError::Corrupt("message time is out of range".to_string()))?;
    record.extend_from_slice(&time.to_le_bytes());
    record.push(state_to_byte(message.state));
    let payload_length =
        u32::try_from(message.payload.len()).map_err(|_| PersistError::TooLarge)?;
    record.extend_from_slice(&payload_length.to_le_bytes());
    record.extend_from_slice(message.payload.as_slice());
    Ok(record)
}

fn decode_new(body: &[u8]) -> Result<(ServiceKey, Message), PersistError> {
    let mut cursor = Cursor::new(body);
    let protocol = read_string(&mut cursor)?;
    let transport = read_string(&mut cursor)?;
    let entry = read_string(&mut cursor)?;
    let mut uuid = [0_u8; 16];
    cursor.read_exact(&mut uuid).map_err(corrupt)?;
    let time = read_i64(&mut cursor)?;
    let state = byte_to_state(read_u8(&mut cursor)?)?;
    let payload_length = read_u32(&mut cursor)? as usize;
    let remaining = body.len().saturating_sub(cursor.position() as usize);
    if payload_length > remaining {
        return Err(PersistError::Corrupt(
            "a message payload is truncated".to_string(),
        ));
    }
    let mut payload = vec![0_u8; payload_length];
    cursor.read_exact(&mut payload).map_err(corrupt)?;

    let seconds = time.div_euclid(1_000_000_000);
    let nanoseconds = time.rem_euclid(1_000_000_000) as u32;
    let time = Utc
        .timestamp_opt(seconds, nanoseconds)
        .single()
        .ok_or_else(|| PersistError::Corrupt("message time is out of range".to_string()))?;
    let key = ServiceKey {
        protocol,
        transport,
        entry,
    };
    Ok((
        key.clone(),
        Message::from_key(Uuid::from_bytes(uuid), time, &key, state, payload),
    ))
}

/// Buffered append handle. The gateway flushes on its timer and on shutdown.
pub struct LogWriter {
    path: PathBuf,
    file: BufWriter<fs::File>,
    file_mode: Option<FileMode>,
    size: u64,
}

impl LogWriter {
    /// Opens an existing HLOG v2 file or creates a new one. A corrupt or v1 file
    /// is reset when `on_corrupt` is `Ignore`; with `Fail` the error is
    /// returned.
    pub fn open(
        path: &Path,
        file_mode: Option<FileMode>,
        on_corrupt: hodeco_contract::config::CorruptPolicy,
    ) -> Result<Self, PersistError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent)?;
        }

        let valid = match fs::read(path) {
            // Validate the whole log, not just the header. A crash can leave a
            // valid header followed by a corrupt middle record, and appending
            // to that file would make the corruption permanent.
            Ok(data) => decode(&data).is_ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let encoded = header(Utc::now().timestamp_nanos_opt().unwrap_or(0));
                write_atomic(path, &encoded, file_mode)?;
                true
            }
            Err(error) => return Err(PersistError::Io(error)),
        };
        if !valid {
            match on_corrupt {
                hodeco_contract::config::CorruptPolicy::Ignore => {
                    let encoded = header(Utc::now().timestamp_nanos_opt().unwrap_or(0));
                    write_atomic(path, &encoded, file_mode)?;
                }
                hodeco_contract::config::CorruptPolicy::Fail => {
                    return Err(PersistError::Corrupt(
                        "the message log is not HLOG v2".to_string(),
                    ));
                }
            }
        } else if let Ok(data) = fs::read(path) {
            // Drop a partial trailing record before appending. Without this,
            // the next record would start inside the partial frame.
            let mut offset = HEADER_LEN;
            while offset + RECORD_OVERHEAD <= data.len() {
                let length =
                    u32::from_le_bytes(data[offset + 1..offset + 5].try_into().unwrap()) as usize;
                let end = offset + RECORD_OVERHEAD + length;
                if end > data.len() {
                    break;
                }
                offset = end;
            }
            if offset < data.len() {
                let file = fs::OpenOptions::new().write(true).open(path)?;
                file.set_len(offset as u64)?;
            }
        }

        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file: BufWriter::new(file),
            file_mode,
            size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn file_mode(&self) -> Option<FileMode> {
        self.file_mode
    }

    pub fn append(&mut self, event: &StoreEvent) -> Result<(), PersistError> {
        let (record_type, body) = match event {
            StoreEvent::New { key, message } => (RECORD_NEW, encode_new(key, message)?),
            StoreEvent::StateChanged { message, .. } => {
                let mut body = Vec::with_capacity(17);
                body.extend_from_slice(message.id.as_bytes());
                body.push(state_to_byte(message.state));
                (RECORD_STATE, body)
            }
            StoreEvent::Dropped { message_ids } => {
                let count = u16::try_from(message_ids.len()).map_err(|_| PersistError::TooLarge)?;
                let mut body = Vec::with_capacity(2 + message_ids.len() * 16);
                body.extend_from_slice(&count.to_le_bytes());
                for id in message_ids {
                    body.extend_from_slice(id.as_bytes());
                }
                (RECORD_DROPPED, body)
            }
        };
        self.file.write_all(&[record_type])?;
        self.file.write_all(&(body.len() as u32).to_le_bytes())?;
        self.file.write_all(&body)?;
        self.size += (RECORD_OVERHEAD + body.len()) as u64;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), PersistError> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        Ok(())
    }
}

fn write_string(data: &mut Vec<u8>, value: &str) -> Result<(), PersistError> {
    let length = u32::try_from(value.len()).map_err(|_| PersistError::TooLarge)?;
    data.extend_from_slice(&length.to_le_bytes());
    data.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, PersistError> {
    let mut value = [0_u8; 1];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    Ok(value[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16, PersistError> {
    let mut value = [0_u8; 2];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, PersistError> {
    let mut value = [0_u8; 4];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    Ok(u32::from_le_bytes(value))
}

fn read_i64(cursor: &mut Cursor<&[u8]>) -> Result<i64, PersistError> {
    let mut value = [0_u8; 8];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    Ok(i64::from_le_bytes(value))
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> Result<String, PersistError> {
    let length = read_u32(cursor)? as usize;
    let mut value = vec![0_u8; length];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    String::from_utf8(value)
        .map_err(|_| PersistError::Corrupt("a service string is not valid UTF-8".to_string()))
}

fn corrupt(error: io::Error) -> PersistError {
    PersistError::Corrupt(error.to_string())
}

fn state_to_byte(state: MessageState) -> u8 {
    state as u8
}

fn byte_to_state(byte: u8) -> Result<MessageState, PersistError> {
    MessageState::try_from(byte).map_err(PersistError::Corrupt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use hodeco_contract::domain::MessageState;
    use hodeco_contract::utils::Size;

    fn key(entry: &str) -> ServiceKey {
        ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: entry.to_string(),
        }
    }

    struct TestFile(PathBuf);

    impl TestFile {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("hodeco-{}.hlog", Uuid::new_v4())))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn populated_store() -> MessageStore {
        let store = MessageStore::new(1024 * 1024);
        let first = key("COM3");
        let second = key("COM4");
        store.register(&first, &[1, 2, 3], MessageState::Incoming);
        let pending = store.register(&second, &[4, 5], MessageState::OutgoingPending);
        store.update_state(pending.id, MessageState::Outgoing);
        store.register(&first, &[], MessageState::OutgoingTimedOut);
        store
    }

    #[test]
    fn round_trips_snapshots() {
        let file = TestFile::new();
        let store = populated_store();

        save(file.path(), &store, Some(FileMode::new(0o640))).unwrap();
        let loaded = load(file.path()).unwrap().expect("snapshot must exist");

        let (entries, _) = store.snapshot();
        assert_eq!(loaded.entries, entries);
        assert_eq!(loaded.bytes, store.bytes());

        let restored = MessageStore::new(Size::from_bytes(1024 * 1024).as_bytes());
        restored.replace_all(loaded.entries.clone(), loaded.bytes);
        assert_eq!(restored.bytes(), store.bytes());
        assert_eq!(restored.len(), store.len());
    }

    #[test]
    fn round_trips_times_with_nanosecond_precision() {
        let mut entries = Vec::new();
        let message = Message::from_key(
            Uuid::new_v4(),
            Utc::now() + ChronoDuration::nanoseconds(123),
            &key("COM3"),
            MessageState::Incoming,
            vec![1],
        );
        entries.push((key("COM3"), message.clone()));

        let data = encode(&entries).unwrap();
        let decoded = decode(&data).unwrap();
        assert_eq!(decoded.entries[0].1.time, message.time);
        assert_eq!(decoded.entries[0].1.id, message.id);
    }

    #[test]
    fn appends_and_replays_new_state_and_dropped_records() {
        let file = TestFile::new();
        let store = populated_store();
        let mut writer = LogWriter::open(
            file.path(),
            None,
            hodeco_contract::config::CorruptPolicy::Ignore,
        )
        .unwrap();
        for (key, message) in store.snapshot().0 {
            writer.append(&StoreEvent::New { key, message }).unwrap();
        }
        writer.flush().unwrap();

        let loaded = load(file.path()).unwrap().unwrap();
        let (entries, _) = store.snapshot();
        assert_eq!(loaded.entries, entries);

        // New messages continue from the same log.
        let mut writer = LogWriter::open(
            file.path(),
            None,
            hodeco_contract::config::CorruptPolicy::Ignore,
        )
        .unwrap();
        let extra = store.register(&key("COM5"), &[9], MessageState::Incoming);
        writer
            .append(&StoreEvent::New {
                key: key("COM5"),
                message: extra.clone(),
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let loaded = load(file.path()).unwrap().unwrap();
        assert!(
            loaded
                .entries
                .iter()
                .any(|(_, message)| message.id == extra.id)
        );
    }

    #[test]
    fn saves_missing_snapshots_as_none() {
        let file = TestFile::new();
        assert!(load(file.path()).unwrap().is_none());
    }

    #[test]
    fn rejects_paths_without_file_names() {
        let store = MessageStore::new(1024 * 1024);

        let error = save(Path::new(".."), &store, None).unwrap_err();
        assert!(matches!(error, PersistError::InvalidPath(_)));
    }

    #[test]
    fn overwrites_existing_snapshots() {
        let file = TestFile::new();
        let empty = MessageStore::new(1024 * 1024);
        save(file.path(), &empty, None).unwrap();
        assert!(load(file.path()).unwrap().unwrap().entries.is_empty());

        let store = populated_store();
        save(file.path(), &store, None).unwrap();
        assert_eq!(
            load(file.path()).unwrap().unwrap().entries.len(),
            store.len()
        );
    }

    #[test]
    fn rejects_corrupt_and_v1_snapshots() {
        let file = TestFile::new();
        let store = populated_store();
        save(file.path(), &store, None).unwrap();
        let data = fs::read(file.path()).unwrap();

        assert!(matches!(decode(&[]), Err(PersistError::Corrupt(_))));
        assert!(matches!(decode(b"NOPE"), Err(PersistError::Corrupt(_))));

        let mut v1 = data.clone();
        v1[4] = 1;
        let error = decode(&v1).unwrap_err().to_string();
        assert!(error.contains("HLOG v1"), "{error}");

        let mut wrong_magic = data.clone();
        wrong_magic[0] = b'X';
        assert!(matches!(
            decode(&wrong_magic),
            Err(PersistError::Corrupt(_))
        ));

        // A partial trailing record is ignored.
        let mut truncated = data.clone();
        truncated.truncate(data.len() - 1);
        assert!(decode(&truncated).is_ok());

        let mut bad_state = data.clone();
        // Last record: ... time (8), state (1), payload length (4), no payload.
        let last_state = bad_state.len() - 5;
        bad_state[last_state] = 9;
        assert!(matches!(decode(&bad_state), Err(PersistError::Corrupt(_))));
    }
}

#[cfg(test)]
mod tail_tests {
    use super::*;
    use hodeco_contract::domain::MessageState;

    #[test]
    fn open_truncates_a_partial_trailing_record_before_appending() {
        let file = std::env::temp_dir().join(format!("hodeco-tail-{}.hlog", Uuid::new_v4()));
        let mut entries = Vec::new();
        let key = ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM3".to_string(),
        };
        let message = Message::from_key(
            Uuid::new_v4(),
            Utc::now(),
            &key,
            MessageState::Incoming,
            vec![1, 2, 3],
        );
        entries.push((key.clone(), message));
        let data = encode(&entries).unwrap();
        fs::write(&file, &data[..data.len() - 1]).unwrap();

        let mut writer =
            LogWriter::open(&file, None, hodeco_contract::config::CorruptPolicy::Ignore).unwrap();
        let next = Message::from_key(
            Uuid::new_v4(),
            Utc::now(),
            &key,
            MessageState::Incoming,
            vec![4],
        );
        writer
            .append(&StoreEvent::New {
                key: key.clone(),
                message: next.clone(),
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let loaded = load(&file).unwrap().unwrap();
        // The truncated original record was not recoverable, but the new
        // record is framed correctly after it.
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].1.id, next.id);
        let _ = fs::remove_file(&file);
    }
}
