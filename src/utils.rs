use hodeco_contract::utils::FileMode;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Returns the current local time formatted as `YYYY-MM-DD HH:MM:SS`.
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Writes `data` to `path` atomically through a temporary file in the same
/// directory, creating parent directories as needed. `mode` sets the file
/// permissions on Unix; it is ignored elsewhere. The path must include a file
/// name.
pub(crate) fn write_atomic(path: &Path, data: &[u8], mode: Option<FileMode>) -> io::Result<()> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the atomic write path must include a file name",
        )
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));

    let result = (|| -> io::Result<()> {
        let file = std::fs::File::create(&temporary)?;
        if let Some(mode) = mode {
            apply_mode(&file, mode)?;
        }
        let mut writer = BufWriter::new(file);
        writer.write_all(data)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        std::fs::rename(&temporary, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn apply_mode(file: &std::fs::File, mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(mode.as_u32()))
}

#[cfg(not(unix))]
fn apply_mode(_file: &std::fs::File, _mode: FileMode) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps() {
        let timestamp = timestamp();
        assert_eq!(timestamp.len(), 19);
        assert_eq!(&timestamp[4..5], "-");
        assert_eq!(&timestamp[7..8], "-");
        assert_eq!(&timestamp[10..11], " ");
        assert_eq!(&timestamp[13..14], ":");
        assert_eq!(&timestamp[16..17], ":");
    }

    #[test]
    fn writes_files_atomically() {
        let root = std::env::temp_dir().join(format!("hodeco-atomic-{}", uuid::Uuid::new_v4()));
        let file = root.join("nested").join("config.yaml");
        write_atomic(&file, b"content", None).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"content");

        let entries: Vec<String> = std::fs::read_dir(root.join("nested"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["config.yaml".to_string()]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_paths_without_file_names() {
        let root = std::env::temp_dir();
        assert!(write_atomic(&root, b"content", None).is_err());
    }
}
