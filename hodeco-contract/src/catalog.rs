//! Static protocol and transport metadata shared by the gateway and the UI.
//!
//! The gateway is the source of truth for runtime behavior; this module keeps
//! the protocol, transport, and service parameter definitions in one place so
//! the demo transport cannot drift from the real service.

use crate::domain::{ParamDef, ParamType, ProtocolInfo, TransportInfo};
use serde_json::json;

pub const PROTOCOL_ZWAVE: &str = "zwave";
pub const PROTOCOL_ZIGBEE: &str = "zigbee";
pub const TRANSPORT_SERIAL: &str = "serial";

pub const SERIAL_PARAM_BAUD_RATE: &str = "baudRate";
pub const SERIAL_PARAM_DATA_BITS: &str = "dataBits";
pub const SERIAL_PARAM_PARITY: &str = "parity";
pub const SERIAL_PARAM_STOP_BITS: &str = "stopBits";
pub const SERIAL_PARAM_FLOW_CONTROL: &str = "flowControl";
pub const SERIAL_PARAM_READ_TIMEOUT: &str = "readTimeout";
pub const SERIAL_PARAM_WRITE_TIMEOUT: &str = "writeTimeout";

pub const SERVICE_PARAM_OPEN_ATTEMPTS_INTERVAL: &str = "openAttemptsInterval";
pub const SERVICE_PARAM_OUTGOING_MAX_TTL: &str = "outgoingMaxTTL";

pub const ZIGBEE_PARAM_ASH_ACK_TIMEOUT: &str = "ashAckTimeout";
pub const ZIGBEE_PARAM_ASH_MAX_RETRIES: &str = "ashMaxRetries";

fn enum_def(name: &str, default: &str, values: &[&str], description: &str) -> ParamDef {
    let mut definition =
        ParamDef::with_default(name, ParamType::String, json!(default), description);
    definition.enum_values = values.iter().map(|value| json!(value)).collect();
    definition
}

pub fn serial_params() -> Vec<ParamDef> {
    vec![
        ParamDef::with_default(
            SERIAL_PARAM_BAUD_RATE,
            ParamType::Uint,
            json!(115_200),
            "Serial port baud rate",
        ),
        enum_def(
            SERIAL_PARAM_DATA_BITS,
            "8",
            &["5", "6", "7", "8"],
            "Number of data bits per character",
        ),
        enum_def(
            SERIAL_PARAM_PARITY,
            "none",
            &["none", "odd", "even"],
            "Parity mode",
        ),
        enum_def(
            SERIAL_PARAM_STOP_BITS,
            "1",
            &["1", "2"],
            "Number of stop bits",
        ),
        enum_def(
            SERIAL_PARAM_FLOW_CONTROL,
            "none",
            &["none", "software", "hardware"],
            "Serial flow control mode",
        ),
        ParamDef::with_default(
            SERIAL_PARAM_READ_TIMEOUT,
            ParamType::Duration,
            json!("0 s"),
            "Maximum wait for a serial reply; zero uses the protocol default",
        ),
        ParamDef::with_default(
            SERIAL_PARAM_WRITE_TIMEOUT,
            ParamType::Duration,
            json!("0 s"),
            "Maximum serial write duration; zero disables the write timeout",
        ),
    ]
}

fn service_params() -> Vec<ParamDef> {
    vec![
        ParamDef::with_default(
            SERVICE_PARAM_OPEN_ATTEMPTS_INTERVAL,
            ParamType::Duration,
            json!("3 s"),
            "Interval between serial port open attempts",
        ),
        ParamDef::with_default(
            SERVICE_PARAM_OUTGOING_MAX_TTL,
            ParamType::Duration,
            json!("10 s"),
            "Maximum time an outgoing message may wait in the send queue",
        ),
    ]
}

pub fn zwave_params() -> Vec<ParamDef> {
    service_params()
}

pub fn zigbee_params() -> Vec<ParamDef> {
    let mut retries = ParamDef::with_default(
        ZIGBEE_PARAM_ASH_MAX_RETRIES,
        ParamType::Uint,
        json!(5),
        "Maximum ASH transmit retries before the link is restarted",
    );
    retries.min = Some(json!(1));
    retries.max = Some(json!(20));

    let mut ack_timeout = ParamDef::with_default(
        ZIGBEE_PARAM_ASH_ACK_TIMEOUT,
        ParamType::Duration,
        json!("1.6 s"),
        "Maximum wait for an ASH acknowledgement; doubles after a timeout",
    );
    ack_timeout.min = Some(json!("100 ms"));
    ack_timeout.max = Some(json!("10 s"));

    let mut definitions = service_params();
    definitions.push(ack_timeout);
    definitions.push(retries);
    definitions
}

pub fn serial_transport_info() -> TransportInfo {
    TransportInfo {
        id: TRANSPORT_SERIAL.to_string(),
        name: "Serial".to_string(),
        description: Some("Serial port transport".to_string()),
        params: serial_params(),
    }
}

pub fn zwave_protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        id: PROTOCOL_ZWAVE.to_string(),
        name: "Z-Wave".to_string(),
        description: Some("Raw Z-Wave serial frame relay".to_string()),
        transports: vec![TRANSPORT_SERIAL.to_string()],
        params: zwave_params(),
    }
}

pub fn zigbee_protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        id: PROTOCOL_ZIGBEE.to_string(),
        name: "ZigBee".to_string(),
        description: Some("Ember EZSP serial frame relay".to_string()),
        transports: vec![TRANSPORT_SERIAL.to_string()],
        params: zigbee_params(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_zwave_serial_metadata() {
        let protocol = zwave_protocol_info();
        assert_eq!(protocol.id, PROTOCOL_ZWAVE);
        assert_eq!(protocol.transports, vec![TRANSPORT_SERIAL]);

        let transport = serial_transport_info();
        assert_eq!(transport.id, TRANSPORT_SERIAL);
        assert_eq!(serial_params().len(), 7);
        assert_eq!(zwave_params().len(), 2);
    }

    #[test]
    fn exposes_zigbee_serial_metadata() {
        let protocol = zigbee_protocol_info();
        assert_eq!(protocol.id, PROTOCOL_ZIGBEE);
        assert_eq!(protocol.name, "ZigBee");
        assert_eq!(
            protocol.description.as_deref(),
            Some("Ember EZSP serial frame relay")
        );
        assert_eq!(protocol.transports, vec![TRANSPORT_SERIAL]);
        assert_eq!(zigbee_params().len(), 4);
    }
}
