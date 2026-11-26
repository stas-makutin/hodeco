//! Shared entity catalog for service-specific device labels.
//!
//! The bundled JSON file is protocol-neutral at the top level: devices have
//! stable catalog IDs and services map their native IDs to those devices. The
//! UI only uses the catalog ID as a join key; it is never shown to the user.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

/// Prefix used for service entries addressed by alias.
pub fn alias_service_key(alias: &str) -> String {
    format!("alias:{alias}")
}

/// Prefix used for service entries addressed by their stable composite key.
pub fn composite_service_key(protocol: &str, transport: &str, entry: &str) -> String {
    format!("key:{protocol}/{transport}/{entry}")
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityCatalog {
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceEntities>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Device {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub location: Option<String>,
    /// Unknown fields are preserved so other applications can reuse the file.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Device {
    pub fn display(&self) -> String {
        match &self.location {
            Some(location) if !location.is_empty() => format!("{} ({location})", self.label),
            _ => self.label.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServiceEntities {
    #[serde(default)]
    pub devices: Vec<ServiceDevice>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDevice {
    #[serde(alias = "device_id")]
    pub device_id: String,
    #[serde(alias = "service_id")]
    pub service_id: ServiceId,
    /// Stable ZigBee EUI-64 identity. Optional for Z-Wave.
    #[serde(default)]
    pub eui64: Option<String>,
    /// ZigBee application endpoint on the mapped node.
    #[serde(default)]
    pub endpoint: Option<u8>,
    /// ZigBee application profile, when the mapping is endpoint-specific.
    #[serde(default)]
    pub profile: Option<u16>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Native address of a device within one service. Z-Wave uses numeric node IDs;
/// other protocols may use strings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ServiceId {
    Number(u64),
    Text(String),
}

impl ServiceId {
    pub fn as_node_id(&self) -> Option<u8> {
        match self {
            Self::Number(value) => u8::try_from(*value).ok(),
            Self::Text(_) => None,
        }
    }

    /// Parses a 16-bit network address from either a JSON number or a string
    /// such as `0x1234` or `1234`.
    pub fn as_u16_address(&self) -> Option<u16> {
        match self {
            Self::Number(value) => u16::try_from(*value).ok(),
            Self::Text(text) => parse_u16(text),
        }
    }
}

fn parse_u16(text: &str) -> Option<u16> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<u16>().ok()
    }
}

impl ServiceDevice {
    pub fn network_address(&self) -> Option<u16> {
        self.service_id.as_u16_address()
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => write!(formatter, "{value}"),
            Self::Text(value) => formatter.write_str(value),
        }
    }
}

impl EntityCatalog {
    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.id == id)
    }

    /// Resolves a native service ID through the ordered service keys. The
    /// caller passes composite keys first and alias keys second.
    pub fn service_device(
        &self,
        service_keys: &[String],
        service_id: &ServiceId,
    ) -> Option<&Device> {
        for key in service_keys {
            let Some(service) = self.services.get(key) else {
                continue;
            };
            let Some(entry) = service
                .devices
                .iter()
                .find(|entry| &entry.service_id == service_id)
            else {
                continue;
            };
            if let Some(device) = self.device(&entry.device_id) {
                return Some(device);
            }
        }
        None
    }

    pub fn node_label(&self, service_keys: &[String], node: u8) -> Option<&Device> {
        self.service_device(service_keys, &ServiceId::Number(u64::from(node)))
    }

    /// Resolves a ZigBee NWK address through either numeric or textual
    /// `serviceId` mappings.
    pub fn network_device(&self, service_keys: &[String], address: u16) -> Option<&Device> {
        for key in service_keys {
            let Some(service) = self.services.get(key) else {
                continue;
            };
            let Some(entry) = service
                .devices
                .iter()
                .find(|entry| entry.network_address() == Some(address))
            else {
                continue;
            };
            if let Some(device) = self.device(&entry.device_id) {
                return Some(device);
            }
        }
        None
    }

    /// Returns the service mappings with their resolved catalog devices.
    pub fn service_devices(&self, service_keys: &[String]) -> Vec<(&ServiceDevice, &Device)> {
        let mut result = Vec::new();
        for key in service_keys {
            let Some(service) = self.services.get(key) else {
                continue;
            };
            for entry in &service.devices {
                if let Some(device) = self.device(&entry.device_id) {
                    result.push((entry, device));
                }
            }
        }
        result
    }
}

static INSTALLED: OnceLock<RwLock<Option<Arc<EntityCatalog>>>> = OnceLock::new();

fn installed() -> &'static RwLock<Option<Arc<EntityCatalog>>> {
    INSTALLED.get_or_init(|| RwLock::new(None))
}

/// Parsed bundled `data/entities.json`.
///
/// This is the compile-time fallback used when no external catalog was
/// installed at startup.
pub fn bundled() -> &'static EntityCatalog {
    static CATALOG: OnceLock<EntityCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../data/entities.json"))
            .expect("bundled entities.json must be valid")
    })
}

fn bundled_shared() -> Arc<EntityCatalog> {
    static SHARED: OnceLock<Arc<EntityCatalog>> = OnceLock::new();
    Arc::clone(SHARED.get_or_init(|| Arc::new(bundled().clone())))
}

/// Installs an externally loaded entity catalog.
///
/// The UI calls this once at startup after reading `entities.json` next to the
/// executable or the web bundle. Invalid JSON is rejected and the embedded
/// fallback stays active.
pub fn install(json: &str) -> Result<(), String> {
    let catalog: EntityCatalog =
        serde_json::from_str(json).map_err(|error| format!("invalid entity catalog: {error}"))?;
    *installed()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(catalog));
    Ok(())
}

/// The catalog currently in use: the installed file when present, or the
/// embedded fallback.
pub fn current() -> Arc<EntityCatalog> {
    installed()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(bundled_shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_parses() {
        let catalog = bundled();
        assert_eq!(catalog.version, 1);
        assert!(!catalog.devices.is_empty());
        assert!(!catalog.services.is_empty());
    }

    #[test]
    fn resolves_alias_and_composite_service_keys() {
        let catalog = bundled();
        let alias = vec![
            composite_service_key("zwave", "serial", "COM4"),
            alias_service_key("ZW"),
        ];
        let device = catalog.node_label(&alias, 2).expect("node 2 through alias");
        assert_eq!(device.label, "Office lamp");

        let composite = vec![composite_service_key("zwave", "serial", "COM3")];
        let device = catalog
            .node_label(&composite, 3)
            .expect("node 3 through composite key");
        assert_eq!(device.label, "Hallway dimmer");
    }

    #[test]
    fn resolves_zigbee_nwk_and_endpoint_metadata() {
        let catalog = bundled();
        let keys = vec![alias_service_key("ZB")];
        let device = catalog.network_device(&keys, 0x1234).expect("NWK 0x1234");
        assert_eq!(device.label, "Office lamp");
        assert_eq!(
            catalog
                .network_device(&keys, 0x1237)
                .expect("NWK 0x1237")
                .label,
            "Kitchen light"
        );

        let entry = catalog
            .services
            .get("alias:ZB")
            .and_then(|service| {
                service
                    .devices
                    .iter()
                    .find(|entry| entry.network_address() == Some(0x1234))
            })
            .expect("mapped NWK entry");
        assert_eq!(entry.endpoint, Some(1));
    }

    #[test]
    fn accepts_legacy_snake_case_service_fields() {
        let catalog: EntityCatalog = serde_json::from_str(
            r#"{
                "version": 1,
                "devices": [{ "id": "a", "label": "A" }],
                "services": {
                    "alias:X": {
                        "devices": [{ "device_id": "a", "service_id": 7 }]
                    }
                }
            }"#,
        )
        .unwrap();
        let device = catalog
            .service_device(&[alias_service_key("X")], &ServiceId::Number(7))
            .expect("legacy mapping");
        assert_eq!(device.label, "A");
    }

    #[test]
    fn composite_key_wins_over_alias() {
        let catalog: EntityCatalog = serde_json::from_str(
            r#"{
                "version": 1,
                "devices": [
                    { "id": "a", "label": "Composite" },
                    { "id": "b", "label": "Alias" }
                ],
                "services": {
                    "key:p/t/e": { "devices": [{ "deviceId": "a", "serviceId": 1 }] },
                    "alias:X": { "devices": [{ "deviceId": "b", "serviceId": 1 }] }
                }
            }"#,
        )
        .unwrap();
        let keys = vec![composite_service_key("p", "t", "e"), alias_service_key("X")];
        assert_eq!(catalog.node_label(&keys, 1).unwrap().label, "Composite");
    }

    #[test]
    fn unknown_devices_have_no_label() {
        let catalog = bundled();
        let keys = vec![alias_service_key("ZW")];
        assert!(catalog.node_label(&keys, 200).is_none());
        assert!(
            catalog
                .node_label(&[alias_service_key("missing")], 2)
                .is_none()
        );
    }

    #[test]
    fn preserves_unknown_fields() {
        let catalog: EntityCatalog = serde_json::from_str(
            r#"{
                "version": 1,
                "devices": [{ "id": "a", "label": "A", "vendor": "Example" }],
                "services": {
                    "alias:X": {
                        "devices": [{ "deviceId": "a", "serviceId": "addr-1", "note": "x" }]
                    }
                }
            }"#,
        )
        .unwrap();
        assert_eq!(catalog.devices[0].extra["vendor"], "Example");
        assert_eq!(catalog.services["alias:X"].devices[0].extra["note"], "x");
        let device = catalog
            .service_device(
                &[alias_service_key("X")],
                &ServiceId::Text("addr-1".to_string()),
            )
            .unwrap();
        assert_eq!(device.label, "A");
    }
}
