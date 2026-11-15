use super::ServiceError;
use hodeco_contract::catalog as shared;
use hodeco_contract::domain::{
    ParamDef, ParamValues, ProtocolInfo, TransportInfo, parse_param_values,
};

pub use hodeco_contract::catalog::{
    PROTOCOL_ZWAVE, SERIAL_PARAM_BAUD_RATE, SERIAL_PARAM_DATA_BITS, SERIAL_PARAM_PARITY,
    SERIAL_PARAM_READ_TIMEOUT, SERIAL_PARAM_STOP_BITS, SERIAL_PARAM_WRITE_TIMEOUT,
    TRANSPORT_SERIAL, ZWAVE_PARAM_OPEN_ATTEMPTS_INTERVAL, ZWAVE_PARAM_OUTGOING_MAX_TTL,
};

/// Transport IDs a protocol can use. `None` means the protocol is unknown.
pub fn supported_transports(protocol: &str) -> Option<&'static [&'static str]> {
    match protocol {
        PROTOCOL_ZWAVE => Some(&[TRANSPORT_SERIAL]),
        _ => None,
    }
}

pub fn supports_transport(protocol: &str, transport: &str) -> bool {
    supported_transports(protocol).is_some_and(|transports| transports.contains(&transport))
}

pub fn protocol_transport_params(
    protocol: &str,
    transport: &str,
) -> Result<Vec<ParamDef>, ServiceError> {
    if supported_transports(protocol).is_none() {
        return Err(ServiceError::UnknownProtocol(protocol.to_string()));
    }
    if transport_info(transport).is_none() {
        return Err(ServiceError::UnknownTransport(transport.to_string()));
    }
    if !supports_transport(protocol, transport) {
        return Err(ServiceError::UnsupportedTransport {
            protocol: protocol.to_string(),
            transport: transport.to_string(),
        });
    }

    let mut definitions = shared::serial_params();
    definitions.extend(shared::zwave_params());
    Ok(definitions)
}

pub fn validate_service(
    protocol: &str,
    transport: &str,
    params: &ParamValues,
) -> Result<ParamValues, ServiceError> {
    let definitions = protocol_transport_params(protocol, transport)?;
    Ok(parse_param_values(&definitions, params)?)
}

pub fn transport_info(id: &str) -> Option<TransportInfo> {
    (id == TRANSPORT_SERIAL).then(shared::serial_transport_info)
}

pub fn transport_infos() -> Vec<TransportInfo> {
    vec![transport_info(TRANSPORT_SERIAL).expect("serial transport is defined")]
}

pub fn protocol_info(id: &str) -> Option<ProtocolInfo> {
    (id == PROTOCOL_ZWAVE).then(shared::zwave_protocol_info)
}

pub fn protocol_infos() -> Vec<ProtocolInfo> {
    vec![protocol_info(PROTOCOL_ZWAVE).expect("zwave protocol is defined")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exposes_protocol_and_transport_info() {
        let protocols = protocol_infos();
        assert_eq!(protocols.len(), 1);
        assert_eq!(protocols[0].id, PROTOCOL_ZWAVE);
        assert_eq!(protocols[0].name, "Z-Wave");

        assert_eq!(protocols[0].transports, vec![TRANSPORT_SERIAL]);

        let transports = transport_infos();
        assert_eq!(transports.len(), 1);
        assert_eq!(transports[0].id, TRANSPORT_SERIAL);
        assert_eq!(transports[0].name, "Serial");
    }

    #[test]
    fn rejects_unknown_protocols_and_transports() {
        assert!(matches!(
            protocol_transport_params("unknown", TRANSPORT_SERIAL),
            Err(ServiceError::UnknownProtocol(_))
        ));
        assert!(matches!(
            protocol_transport_params(PROTOCOL_ZWAVE, "unknown"),
            Err(ServiceError::UnknownTransport(_))
        ));
        assert!(supports_transport(PROTOCOL_ZWAVE, TRANSPORT_SERIAL));
        assert!(!supports_transport("unknown", TRANSPORT_SERIAL));
    }

    #[test]
    fn validates_service_parameters() {
        let params = ParamValues::from([
            ("baudRate".to_string(), json!(57_600)),
            ("dataBits".to_string(), json!("7")),
            ("readTimeout".to_string(), json!("5 s")),
        ]);
        let parsed = validate_service(PROTOCOL_ZWAVE, TRANSPORT_SERIAL, &params).unwrap();
        assert_eq!(parsed.get("baudRate"), Some(&json!(57_600)));
        assert_eq!(parsed.get("dataBits"), Some(&json!("7")));
        assert_eq!(parsed.get("readTimeout"), Some(&json!("5 s")));
        assert_eq!(parsed.get("writeTimeout"), Some(&json!("0 s")));
        assert_eq!(parsed.get("openAttemptsInterval"), Some(&json!("3 s")));
        assert_eq!(parsed.get("outgoingMaxTTL"), Some(&json!("10 s")));

        let params = ParamValues::from([("baudRate".to_string(), json!("fast"))]);
        assert!(validate_service(PROTOCOL_ZWAVE, TRANSPORT_SERIAL, &params).is_err());
    }
}
