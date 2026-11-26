//! Manual integration test against a real ZigBee EZSP controller.
//!
//! Run with:
//! `cargo test --test real_zigbee -- --ignored --nocapture`
//!
//! Set `HODECO_ZIGBEE_PORT` to override the default `COM5`.

use hodeco::messages::{MessageFilter, MessageStore};
use hodeco::services::discovery::{DiscoveryBackend, SerialDiscoveryBackend};
use hodeco::services::registry::ServiceRegistry;
use hodeco::services::serial::{
    PortDescriptor, PortEnumerator, SerialSettings, SystemPortOpener, open_serial,
};
use hodeco::services::zigbee::probe_version;
use hodeco_contract::domain::{
    MessageOrder, MessageState, ParamValues, ServiceKey, ServiceRef, ServiceState,
};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio_serial::FlowControl;
use tokio_util::sync::CancellationToken;

/// The hardware tests share one serial port, so libtest's parallel threads must
/// not open it concurrently.
static HARDWARE_PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[ignore = "requires a real ZigBee EZSP controller; set HODECO_ZIGBEE_PORT (default COM5)"]
async fn real_zigbee_controller_responds_to_version_request() {
    let _guard = HARDWARE_PORT.lock().await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let port_name = std::env::var("HODECO_ZIGBEE_PORT").unwrap_or_else(|_| "COM5".to_string());
    let settings = SerialSettings {
        baud_rate: 57_600,
        flow_control: FlowControl::None,
        ..SerialSettings::default()
    };
    let mut port = open_serial(&port_name, &settings)
        .unwrap_or_else(|error| panic!("unable to open '{port_name}': {error}"));

    let version = probe_version(&mut port, Duration::from_secs(3))
        .await
        .unwrap_or_else(|error| panic!("probe failed on '{port_name}': {error}"))
        .unwrap_or_else(|| panic!("no EZSP version response from '{port_name}'"));

    println!(
        "{port_name}: EZSP v{} · stack 0x{:04x}",
        version.protocol, version.stack_version
    );
    assert!(version.protocol >= 4);
}

struct SinglePort(String);

impl PortEnumerator for SinglePort {
    fn ports(&self) -> io::Result<Vec<PortDescriptor>> {
        Ok(vec![PortDescriptor {
            name: self.0.clone(),
            product: Some("Nortec HUSBZB-1 ZigBee Hub".to_string()),
            manufacturer: Some("Silicon Labs".to_string()),
            serial_number: None,
        }])
    }
}

#[tokio::test]
#[ignore = "requires a real ZigBee EZSP controller; set HODECO_ZIGBEE_PORT (default COM5)"]
async fn real_zigbee_discovery_finds_controller() {
    let _guard = HARDWARE_PORT.lock().await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let port_name = std::env::var("HODECO_ZIGBEE_PORT").unwrap_or_else(|_| "COM5".to_string());
    let params =
        hodeco::services::catalog::validate_service("zigbee", "serial", &ParamValues::new())
            .expect("zigbee defaults");
    let backend = SerialDiscoveryBackend::new(
        Arc::new(SystemPortOpener),
        Arc::new(SinglePort(port_name.clone())),
    );
    let entries = backend
        .discover(
            "zigbee".to_string(),
            "serial".to_string(),
            params,
            CancellationToken::new(),
        )
        .await
        .expect("discovery failed");
    println!("{entries:#?}");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].available, "{entries:#?}");
}

#[tokio::test]
#[ignore = "requires a real ZigBee EZSP controller; set HODECO_ZIGBEE_PORT (default COM5)"]
async fn real_zigbee_service_queries_network_state() {
    let _guard = HARDWARE_PORT.lock().await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let port_name = std::env::var("HODECO_ZIGBEE_PORT").unwrap_or_else(|_| "COM5".to_string());
    let store = Arc::new(MessageStore::new(1024 * 1024));
    let cancel = CancellationToken::new();
    let registry = ServiceRegistry::new(store.clone(), Arc::new(SystemPortOpener), &cancel);
    let key = ServiceKey {
        protocol: "zigbee".to_string(),
        transport: "serial".to_string(),
        entry: port_name.clone(),
    };
    let reference = ServiceRef::Key {
        protocol: key.protocol.clone(),
        transport: key.transport.clone(),
        entry: key.entry.clone(),
    };
    let params = ParamValues::from([
        ("baudRate".to_string(), serde_json::json!(57_600)),
        ("flowControl".to_string(), serde_json::json!("none")),
    ]);
    registry
        .add(key, &params, Some("ZB-TEST".to_string()))
        .unwrap();

    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if registry.status(&reference).unwrap().state == ServiceState::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("zigbee service did not start");

    let sent = registry
        .send(&reference, vec![0x21, 0x00, 0x01, 0x18, 0x00], None)
        .expect("send networkState");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .get(sent.id)
                .is_some_and(|message| message.state == MessageState::Outgoing)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("networkState was not acknowledged");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = store.list(
                None,
                None,
                MessageOrder::OldestFirst,
                128,
                &MessageFilter {
                    states: vec![MessageState::Incoming],
                    ..Default::default()
                },
            );
            if page.messages.iter().any(|message| {
                let payload = message.payload.as_slice();
                payload.len() >= 6
                    && payload[1] & 0x80 != 0
                    && payload[3] == 0x18
                    && payload[4] == 0x00
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("networkState response was not recorded");

    registry.remove(&reference).await.unwrap();
    registry.shutdown().await;
    cancel.cancel();
}
