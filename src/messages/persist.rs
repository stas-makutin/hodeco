use super::MessageStore;
use super::store::{message_entry_length, service_entry_length};
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
pub const VERSION: u8 = 1;
const RESERVED: [u8; 3] = [0; 3];
const RECORD_OVERHEAD: usize = 2 + 8 + 16 + 1 + 2;

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
}

pub fn save(
    path: &Path,
    store: &MessageStore,
    file_mode: Option<FileMode>,
) -> Result<(), PersistError> {
    let (entries, _) = store.snapshot();
    let encoded = encode(&entries)?;
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

    let result = write_snapshot(&temporary, &encoded, file_mode)
        .and_then(|()| fs::rename(&temporary, path).map_err(PersistError::from));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_snapshot(
    path: &Path,
    encoded: &[u8],
    file_mode: Option<FileMode>,
) -> Result<(), PersistError> {
    let file = fs::File::create(path)?;
    if let Some(mode) = file_mode {
        apply_mode(&file, mode)?;
    }
    let mut writer = BufWriter::new(file);
    writer.write_all(encoded)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
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

pub fn encode(entries: &[(ServiceKey, Message)]) -> Result<Vec<u8>, PersistError> {
    let mut services: Vec<ServiceKey> = Vec::new();
    let mut service_indices: HashMap<&ServiceKey, u16> = HashMap::new();
    for (key, _) in entries {
        if !service_indices.contains_key(key) {
            let index = u16::try_from(services.len()).map_err(|_| PersistError::TooLarge)?;
            service_indices.insert(key, index);
            services.push(key.clone());
        }
    }

    let service_count = u16::try_from(services.len()).map_err(|_| PersistError::TooLarge)?;
    let record_count = u32::try_from(entries.len()).map_err(|_| PersistError::TooLarge)?;

    let mut data = Vec::new();
    data.extend_from_slice(&MAGIC);
    data.push(VERSION);
    data.extend_from_slice(&RESERVED);
    data.extend_from_slice(&service_count.to_le_bytes());
    for service in &services {
        write_string(&mut data, &service.protocol)?;
        write_string(&mut data, &service.transport)?;
        write_string(&mut data, &service.entry)?;
    }

    data.extend_from_slice(&record_count.to_le_bytes());
    for (key, message) in entries {
        let index = *service_indices
            .get(key)
            .expect("service index must exist for every entry");
        let time = message
            .time
            .timestamp_nanos_opt()
            .ok_or_else(|| PersistError::Corrupt("message time is out of range".to_string()))?;
        let payload_length =
            u16::try_from(message.payload.len()).map_err(|_| PersistError::TooLarge)?;

        data.extend_from_slice(&index.to_le_bytes());
        data.extend_from_slice(&time.to_le_bytes());
        data.extend_from_slice(message.id.as_bytes());
        data.push(state_to_byte(message.state));
        data.extend_from_slice(&payload_length.to_le_bytes());
        data.extend_from_slice(message.payload.as_slice());
    }

    Ok(data)
}

pub fn decode(data: &[u8]) -> Result<PersistedLog, PersistError> {
    let mut cursor = Cursor::new(data);

    let mut magic = [0_u8; 4];
    cursor.read_exact(&mut magic).map_err(corrupt)?;
    if magic != MAGIC {
        return Err(PersistError::Corrupt("invalid magic".to_string()));
    }
    let version = read_u8(&mut cursor)?;
    if version != VERSION {
        return Err(PersistError::Corrupt(format!(
            "unsupported version {version}"
        )));
    }
    let mut reserved = [0_u8; 3];
    cursor.read_exact(&mut reserved).map_err(corrupt)?;

    let service_count = read_u16(&mut cursor)?;
    let mut services = Vec::with_capacity(service_count as usize);
    for _ in 0..service_count {
        let protocol = read_string(&mut cursor)?;
        let transport = read_string(&mut cursor)?;
        let entry = read_string(&mut cursor)?;
        services.push(ServiceKey {
            protocol,
            transport,
            entry,
        });
    }

    let record_count = read_u32(&mut cursor)? as usize;
    let remaining = data.len().saturating_sub(cursor.position() as usize);
    if record_count > remaining / RECORD_OVERHEAD + 1 {
        return Err(PersistError::Corrupt(
            "record count exceeds the file size".to_string(),
        ));
    }

    let mut entries = Vec::with_capacity(record_count);
    for _ in 0..record_count {
        let service_index = read_u16(&mut cursor)? as usize;
        let service = services.get(service_index).ok_or_else(|| {
            PersistError::Corrupt(format!("unknown service index {service_index}"))
        })?;
        let time = read_i64(&mut cursor)?;
        let mut uuid = [0_u8; 16];
        cursor.read_exact(&mut uuid).map_err(corrupt)?;
        let state = byte_to_state(read_u8(&mut cursor)?)?;
        let payload_length = read_u16(&mut cursor)? as usize;
        let payload_remaining = data.len().saturating_sub(cursor.position() as usize);
        if payload_length > payload_remaining {
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

        entries.push((
            service.clone(),
            Message::from_key(Uuid::from_bytes(uuid), time, service, state, payload),
        ));
    }

    let mut trailing = [0_u8; 1];
    match cursor.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => {
            return Err(PersistError::Corrupt(
                "unexpected data after the last record".to_string(),
            ));
        }
        Err(error) => return Err(PersistError::Io(error)),
    }

    let mut bytes = 0_u64;
    for service in &services {
        bytes += service_entry_length(service);
    }
    for (_, message) in &entries {
        bytes += message_entry_length(message.payload.len());
    }

    Ok(PersistedLog { entries, bytes })
}

fn write_string(data: &mut Vec<u8>, value: &str) -> Result<(), PersistError> {
    let length = u16::try_from(value.len()).map_err(|_| PersistError::TooLarge)?;
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
    let length = read_u16(cursor)? as usize;
    let mut value = vec![0_u8; length];
    cursor.read_exact(&mut value).map_err(corrupt)?;
    String::from_utf8(value)
        .map_err(|_| PersistError::Corrupt("a service string is not valid UTF-8".to_string()))
}

fn corrupt(error: io::Error) -> PersistError {
    PersistError::Corrupt(error.to_string())
}

fn state_to_byte(state: MessageState) -> u8 {
    match state {
        MessageState::Incoming => 0,
        MessageState::Outgoing => 1,
        MessageState::OutgoingPending => 2,
        MessageState::OutgoingFailed => 3,
        MessageState::OutgoingRejected => 4,
        MessageState::OutgoingTimedOut => 5,
    }
}

fn byte_to_state(byte: u8) -> Result<MessageState, PersistError> {
    match byte {
        0 => Ok(MessageState::Incoming),
        1 => Ok(MessageState::Outgoing),
        2 => Ok(MessageState::OutgoingPending),
        3 => Ok(MessageState::OutgoingFailed),
        4 => Ok(MessageState::OutgoingRejected),
        5 => Ok(MessageState::OutgoingTimedOut),
        value => Err(PersistError::Corrupt(format!(
            "unknown message state {value}"
        ))),
    }
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
        let page = restored.list(
            None,
            None,
            hodeco_contract::domain::MessageOrder::OldestFirst,
            10,
            &crate::messages::MessageFilter::default(),
        );
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            store
                .list(
                    None,
                    None,
                    hodeco_contract::domain::MessageOrder::OldestFirst,
                    10,
                    &crate::messages::MessageFilter::default(),
                )
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>()
        );
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
        let file_name = file
            .path()
            .file_name()
            .expect("snapshot file name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!(".{file_name}.")))
                .count(),
            0
        );
    }

    #[test]
    fn rejects_corrupt_snapshots() {
        let file = TestFile::new();
        let store = populated_store();
        save(file.path(), &store, None).unwrap();
        let data = fs::read(file.path()).unwrap();

        assert!(matches!(decode(&[]), Err(PersistError::Corrupt(_))));
        assert!(matches!(decode(b"NOPE"), Err(PersistError::Corrupt(_))));

        let mut wrong_version = data.clone();
        wrong_version[4] = VERSION + 1;
        assert!(matches!(
            decode(&wrong_version),
            Err(PersistError::Corrupt(_))
        ));

        let mut wrong_magic = data.clone();
        wrong_magic[0] = b'X';
        assert!(matches!(
            decode(&wrong_magic),
            Err(PersistError::Corrupt(_))
        ));

        let mut truncated = data.clone();
        truncated.truncate(data.len() - 1);
        assert!(matches!(decode(&truncated), Err(PersistError::Corrupt(_))));

        let mut trailing = data.clone();
        trailing.push(0);
        assert!(matches!(decode(&trailing), Err(PersistError::Corrupt(_))));

        let mut bad_state = data.clone();
        let last_state = bad_state.len() - 3;
        bad_state[last_state] = 9;
        assert!(matches!(decode(&bad_state), Err(PersistError::Corrupt(_))));
    }

    #[test]
    fn rejects_oversized_payloads() {
        let message = Message::from_key(
            Uuid::new_v4(),
            Utc::now(),
            &key("COM3"),
            MessageState::Incoming,
            vec![0_u8; 65_536],
        );
        assert!(matches!(
            encode(&[(key("COM3"), message)]),
            Err(PersistError::TooLarge)
        ));
    }

    #[test]
    fn writes_service_tables_once() {
        let store = populated_store();
        let data = encode(&store.snapshot().0).unwrap();
        let decoded = decode(&data).unwrap();
        assert_eq!(decoded.entries.len(), 3);
        assert_eq!(u16::from_le_bytes([data[8], data[9]]), 2);
    }
}
