//! Startup loading of the editable codec definition files.
//!
//! `zwave.json`, `zigbee.json`, and `entities.json` are read at runtime from
//! beside the executable (native) or the web bundle (browser). Editing a file
//! only requires restarting or reloading the UI; the embedded copies remain a
//! fallback when a file is missing or invalid.

use hodeco_codec::{definitions, entities, zigbee};

/// Installs whichever external files were successfully read. Invalid files are
/// logged and the embedded fallback stays active for that file.
pub fn install(zwave_json: Option<&str>, entities_json: Option<&str>, zigbee_json: Option<&str>) {
    if let Some(json) = zwave_json {
        match definitions::install(json) {
            Ok(()) => log::info!("loaded external zwave.json definitions"),
            Err(error) => log::warn!("{error}; using embedded zwave.json"),
        }
    }
    if let Some(json) = entities_json {
        match entities::install(json) {
            Ok(()) => log::info!("loaded external entities.json catalog"),
            Err(error) => log::warn!("{error}; using embedded entities.json"),
        }
    }
    if let Some(json) = zigbee_json {
        match zigbee::install(json) {
            Ok(()) => log::info!("loaded external zigbee.json definitions"),
            Err(error) => log::warn!("{error}; using embedded zigbee.json"),
        }
    }
}

/// Loads files adjacent to the native executable, then from the working
/// directory. During development the checked-in codec data directory is the
/// final fallback.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_adjacent_native() {
    use std::path::{Path, PathBuf};

    let mut directories = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        directories.push(parent.to_path_buf());
    }
    if let Ok(current) = std::env::current_dir() {
        directories.push(current);
    }
    directories.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hodeco-codec/data"));

    let mut zwave = None;
    let mut entities = None;
    let mut zigbee = None;
    for directory in directories {
        if zwave.is_none() {
            zwave = read_optional(&directory.join("zwave.json"));
        }
        if entities.is_none() {
            entities = read_optional(&directory.join("entities.json"));
        }
        if zigbee.is_none() {
            zigbee = read_optional(&directory.join("zigbee.json"));
        }
        if zwave.is_some() && entities.is_some() && zigbee.is_some() {
            break;
        }
    }
    install(zwave.as_deref(), entities.as_deref(), zigbee.as_deref());

    fn read_optional(path: &Path) -> Option<String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                log::warn!("could not read '{}': {error}", path.display());
                None
            }
        }
    }
}

/// Fetches the definition files from the web bundle before the app starts.
#[cfg(target_arch = "wasm32")]
pub async fn install_adjacent_web() {
    let zwave = fetch_text("zwave.json").await;
    let entities = fetch_text("entities.json").await;
    let zigbee = fetch_text("zigbee.json").await;
    install(zwave.as_deref(), entities.as_deref(), zigbee.as_deref());
}

#[cfg(target_arch = "wasm32")]
async fn fetch_text(path: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window()?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str(path))
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    if !response.ok() {
        log::warn!(
            "could not fetch '{path}': HTTP {}; using the embedded copy",
            response.status()
        );
        return None;
    }
    let text = JsFuture::from(response.text().ok()?).await.ok()?;
    text.as_string()
}
