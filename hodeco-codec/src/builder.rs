//! Z-Wave Serial API frame construction.
//!
//! The builder always recomputes the length byte and checksum, so callers only
//! supply the semantic fields. All functions are pure and never panic on bad
//! input: invalid node IDs or oversized payloads produce a [`BuildError`].

use crate::zwave::{
    FRAME_MAX_LENGTH, FRAME_REQUEST, FRAME_SOF, MAX_FRAME_BYTES, SERIAL_GET_VERSION,
    SERIAL_SEND_DATA, SERIAL_SEND_DATA_MULTI, checksum,
};
use thiserror::Error;

/// Serial API transmit option bits.
pub const TX_OPTION_ACK: u8 = 0x01;
pub const TX_OPTION_LOW_POWER: u8 = 0x02;
pub const TX_OPTION_AUTO_ROUTE: u8 = 0x04;
pub const TX_OPTION_NO_ROUTE: u8 = 0x10;
pub const TX_OPTION_EXPLORE: u8 = 0x20;

/// Recommended options for a broadcast: no ACK, no route discovery.
pub const BROADCAST_TX_OPTIONS: u8 = TX_OPTION_NO_ROUTE;

pub const BROADCAST_NODE_ID: u8 = 0xFF;
pub const MIN_NODE_ID: u8 = 1;
pub const MAX_NODE_ID: u8 = 232;
pub const MAX_NODES: usize = 232;

/// Host-side Z-Wave builder operations that are not Serial API commands.
///
/// The universal raw-message operation lives in the UI because it applies to
/// every protocol. These two only make sense for Z-Wave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecialOperation {
    RawFrame,
    Broadcast,
}

impl SpecialOperation {
    pub const ZWAVE: [Self; 2] = [Self::RawFrame, Self::Broadcast];

    pub fn label(self) -> &'static str {
        match self {
            Self::RawFrame => "Raw Serial API frame",
            Self::Broadcast => "ZW_SendData (broadcast)",
        }
    }

    /// Lower values appear first in the Operation picker.
    pub fn priority(self) -> i32 {
        match self {
            Self::RawFrame => 5,
            Self::Broadcast => 30,
        }
    }

    pub fn applies_to(self, protocol: &str) -> bool {
        protocol == "zwave"
    }

    pub fn emphasized(self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
    #[error("a SendData frame needs exactly one node ID")]
    InvalidNode,
    #[error("node IDs must be between {MIN_NODE_ID} and {MAX_NODE_ID} (got {0})")]
    NodeOutOfRange(u8),
    #[error("a SendDataMulti frame needs at least one node")]
    NoNodes,
    #[error("too many target nodes ({0}); the limit is {MAX_NODES}")]
    TooManyNodes(usize),
    #[error("target node {0} is listed more than once")]
    DuplicateNode(u8),
    #[error("command data is empty")]
    EmptyData,
    #[error("command data is too long ({0} bytes); the frame limit is {FRAME_MAX_LENGTH}")]
    DataTooLong(usize),
    #[error("raw frame must start with SOF (0x01)")]
    MissingSof,
    #[error("raw frame must contain a frame type and command")]
    RawTooShort,
    #[error("only request (0x00) and response (0x01) frames can be built")]
    InvalidFrameType(u8),
    #[error("frame is too long ({0} bytes); the limit is {MAX_FRAME_BYTES}")]
    FrameTooLong(usize),
}

/// Builds a complete data frame from a frame type and body. The length byte and
/// checksum are always recalculated.
pub fn build_data_frame(frame_type: u8, body: &[u8]) -> Result<Vec<u8>, BuildError> {
    if frame_type > 0x01 {
        return Err(BuildError::InvalidFrameType(frame_type));
    }
    let length = body.len() + 2; // frame type + body + checksum
    if length > FRAME_MAX_LENGTH {
        return Err(BuildError::FrameTooLong(length + 2));
    }
    let mut frame = Vec::with_capacity(length + 2);
    frame.push(FRAME_SOF);
    frame.push(length as u8);
    frame.push(frame_type);
    frame.extend_from_slice(body);
    frame.push(checksum(&frame[1..]));
    Ok(frame)
}

/// Builds a Serial API request frame for an arbitrary command ID.
pub fn build_request(command: u8, params: &[u8]) -> Result<Vec<u8>, BuildError> {
    let mut body = Vec::with_capacity(1 + params.len());
    body.push(command);
    body.extend_from_slice(params);
    build_data_frame(FRAME_REQUEST, &body)
}

/// Builds a Serial API response frame for an arbitrary command ID.
pub fn build_response(command: u8, params: &[u8]) -> Result<Vec<u8>, BuildError> {
    let mut body = Vec::with_capacity(1 + params.len());
    body.push(command);
    body.extend_from_slice(params);
    build_data_frame(0x01, &body)
}

/// Completes an operator-supplied partial frame.
///
/// Accepts bytes starting with SOF followed by a frame type, command, and
/// parameters. The caller-supplied length byte and checksum are ignored and
/// recalculated, which is what the builder preview promises.
pub fn complete_raw_frame(partial: &[u8]) -> Result<Vec<u8>, BuildError> {
    if partial.first() != Some(&FRAME_SOF) {
        return Err(BuildError::MissingSof);
    }
    if partial.len() < 3 {
        return Err(BuildError::RawTooShort);
    }
    let frame_type = partial[1];
    build_data_frame(frame_type, &partial[2..])
}

/// Builds `ZW_Version` (`0x15`) with no parameters.
pub fn build_get_version() -> Vec<u8> {
    build_request(SERIAL_GET_VERSION, &[]).expect("ZW_Version always fits in a Z-Wave frame")
}

/// Builds a `SendData` (`0x13`) request to one node.
pub fn build_send_data(
    node_id: u8,
    command_class_data: &[u8],
    tx_options: u8,
    callback_id: u8,
) -> Result<Vec<u8>, BuildError> {
    validate_node(node_id)?;
    validate_data(command_class_data)?;
    let params = send_data_params(node_id, command_class_data, tx_options, callback_id)?;
    build_request(SERIAL_SEND_DATA, &params)
}

/// Builds a `SendDataMulti` (`0x14`) request to one or more unique nodes.
pub fn build_send_data_multi(
    node_ids: &[u8],
    command_class_data: &[u8],
    tx_options: u8,
    callback_id: u8,
) -> Result<Vec<u8>, BuildError> {
    validate_nodes(node_ids)?;
    validate_data(command_class_data)?;

    let mut params = Vec::with_capacity(3 + node_ids.len() + command_class_data.len());
    params.push(node_ids.len() as u8);
    params.extend_from_slice(node_ids);
    params.push(command_class_data.len() as u8);
    params.extend_from_slice(command_class_data);
    params.push(tx_options);
    params.push(callback_id);
    build_request(SERIAL_SEND_DATA_MULTI, &params)
}

/// Builds a broadcast: `SendData` to node `0xFF` with ACK off and NO_ROUTE on.
pub fn build_broadcast(command_class_data: &[u8], callback_id: u8) -> Result<Vec<u8>, BuildError> {
    validate_data(command_class_data)?;
    let params = send_data_params(
        BROADCAST_NODE_ID,
        command_class_data,
        BROADCAST_TX_OPTIONS,
        callback_id,
    )?;
    build_request(SERIAL_SEND_DATA, &params)
}

fn validate_node(node_id: u8) -> Result<(), BuildError> {
    if (MIN_NODE_ID..=MAX_NODE_ID).contains(&node_id) {
        Ok(())
    } else {
        Err(BuildError::NodeOutOfRange(node_id))
    }
}

fn validate_nodes(node_ids: &[u8]) -> Result<(), BuildError> {
    if node_ids.is_empty() {
        return Err(BuildError::NoNodes);
    }
    if node_ids.len() > MAX_NODES {
        return Err(BuildError::TooManyNodes(node_ids.len()));
    }
    for &node in node_ids {
        validate_node(node)?;
    }
    for (index, node) in node_ids.iter().enumerate() {
        if node_ids[..index].contains(node) {
            return Err(BuildError::DuplicateNode(*node));
        }
    }
    Ok(())
}

fn validate_data(data: &[u8]) -> Result<(), BuildError> {
    if data.is_empty() {
        return Err(BuildError::EmptyData);
    }
    // SendData metadata is 4 bytes plus the length byte; keep a full frame under
    // the Serial API limit.
    if data.len() > FRAME_MAX_LENGTH - 8 {
        return Err(BuildError::DataTooLong(data.len()));
    }
    Ok(())
}

fn send_data_params(
    node_id: u8,
    data: &[u8],
    tx_options: u8,
    callback_id: u8,
) -> Result<Vec<u8>, BuildError> {
    let mut params = Vec::with_capacity(4 + data.len());
    params.push(node_id);
    params.push(data.len() as u8);
    params.extend_from_slice(data);
    params.push(tx_options);
    params.push(callback_id);
    Ok(params)
}

// ---------------------------------------------------------------------------
// Command-class helpers
// ---------------------------------------------------------------------------

pub const CC_BASIC: u8 = 0x20;
pub const CC_SWITCH_BINARY: u8 = 0x25;
pub const CC_SWITCH_MULTILEVEL: u8 = 0x26;
pub const CC_SCENE_ACTIVATION: u8 = 0x2B;
pub const CC_METER: u8 = 0x32;

pub const BASIC_SET: u8 = 0x01;
pub const BASIC_GET: u8 = 0x02;
pub const BASIC_REPORT: u8 = 0x03;
pub const SWITCH_BINARY_SET: u8 = 0x01;
pub const SWITCH_BINARY_GET: u8 = 0x02;
pub const SWITCH_BINARY_REPORT: u8 = 0x03;
pub const SWITCH_MULTILEVEL_SET: u8 = 0x01;
pub const SWITCH_MULTILEVEL_GET: u8 = 0x02;
pub const SWITCH_MULTILEVEL_REPORT: u8 = 0x03;
pub const SCENE_ACTIVATION_SET: u8 = 0x01;
pub const METER_GET: u8 = 0x01;
pub const METER_REPORT: u8 = 0x02;

pub fn basic_set(value: u8) -> Vec<u8> {
    vec![CC_BASIC, BASIC_SET, value]
}

pub fn basic_get() -> Vec<u8> {
    vec![CC_BASIC, BASIC_GET]
}

pub fn basic_report(value: u8) -> Vec<u8> {
    vec![CC_BASIC, BASIC_REPORT, value]
}

pub fn switch_binary_set(on: bool) -> Vec<u8> {
    vec![CC_SWITCH_BINARY, SWITCH_BINARY_SET, u8::from(on)]
}

pub fn switch_binary_get() -> Vec<u8> {
    vec![CC_SWITCH_BINARY, SWITCH_BINARY_GET]
}

pub fn switch_binary_report(on: bool) -> Vec<u8> {
    vec![CC_SWITCH_BINARY, SWITCH_BINARY_REPORT, u8::from(on)]
}

pub fn switch_multilevel_set(level: u8) -> Vec<u8> {
    vec![CC_SWITCH_MULTILEVEL, SWITCH_MULTILEVEL_SET, level]
}

pub fn switch_multilevel_get() -> Vec<u8> {
    vec![CC_SWITCH_MULTILEVEL, SWITCH_MULTILEVEL_GET]
}

pub fn switch_multilevel_report(level: u8) -> Vec<u8> {
    vec![CC_SWITCH_MULTILEVEL, SWITCH_MULTILEVEL_REPORT, level]
}

pub fn scene_activation_set(scene_id: u8, dimming_duration: u8) -> Vec<u8> {
    vec![
        CC_SCENE_ACTIVATION,
        SCENE_ACTIVATION_SET,
        scene_id,
        dimming_duration,
    ]
}

pub fn meter_get() -> Vec<u8> {
    vec![CC_METER, METER_GET, 0x00]
}

pub fn meter_report(meter_type: u8, value: &[u8]) -> Vec<u8> {
    let mut data = vec![CC_METER, METER_REPORT, meter_type];
    data.extend_from_slice(value);
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zwave;

    #[test]
    fn get_version_matches_the_reference_frame() {
        assert_eq!(build_get_version(), vec![0x01, 0x03, 0x00, 0x15, 0xe9]);
    }

    #[test]
    fn send_data_decodes_round_trip() {
        let frame = build_send_data(5, &switch_binary_set(true), TX_OPTION_ACK, 0x21).unwrap();
        assert_eq!(frame[0], 0x01);
        assert_eq!(frame[1] as usize, frame.len() - 2);

        let decoded = zwave::decode(&frame);
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].checksum_ok, Some(true));
        assert!(decoded.summary.contains("SendData"));
        assert!(decoded.summary.contains("node 5"));
    }

    #[test]
    fn send_data_multi_includes_count_and_nodes() {
        let frame = build_send_data_multi(&[2, 7, 9], &basic_set(0xFF), 0, 1).unwrap();
        let expected_params = &[3, 2, 7, 9, 3, CC_BASIC, BASIC_SET, 0xFF, 0, 1];
        assert_eq!(&frame[4..frame.len() - 1], expected_params);
        assert_eq!(frame[3], SERIAL_SEND_DATA_MULTI);
        assert_eq!(frame[frame.len() - 1], checksum(&frame[1..frame.len() - 1]));
    }

    #[test]
    fn broadcast_is_send_data_to_ff() {
        let frame = build_broadcast(&basic_set(0), 0x33).unwrap();
        assert_eq!(frame[3], SERIAL_SEND_DATA);
        assert_eq!(frame[4], 0xFF);
        assert_eq!(
            &frame[frame.len() - 3..],
            &[
                BROADCAST_TX_OPTIONS,
                0x33,
                checksum(&frame[1..frame.len() - 1])
            ]
        );
    }

    #[test]
    fn node_validation_rejects_invalid_and_duplicate_ids() {
        assert_eq!(
            build_send_data(0, &basic_get(), 0, 1),
            Err(BuildError::NodeOutOfRange(0))
        );
        assert_eq!(
            build_send_data(233, &basic_get(), 0, 1),
            Err(BuildError::NodeOutOfRange(233))
        );
        assert_eq!(
            build_send_data_multi(&[], &basic_get(), 0, 1),
            Err(BuildError::NoNodes)
        );
        assert_eq!(
            build_send_data_multi(&[4, 4], &basic_get(), 0, 1),
            Err(BuildError::DuplicateNode(4))
        );
    }

    #[test]
    fn raw_frames_recompute_length_and_checksum() {
        // Operator pastes a stale length and no checksum.
        let frame = complete_raw_frame(&[0x01, 0x00, 0x15]).unwrap();
        assert_eq!(frame, build_get_version());
        assert_eq!(
            complete_raw_frame(&[0x00, 0x15]),
            Err(BuildError::MissingSof)
        );
        assert_eq!(
            complete_raw_frame(&[0x01, 0x05, 0x15]),
            Err(BuildError::InvalidFrameType(0x05))
        );
    }

    #[test]
    fn command_helpers_use_documented_ids() {
        assert_eq!(basic_set(0x63), [0x20, 0x01, 0x63]);
        assert_eq!(switch_binary_set(false), [0x25, 0x01, 0x00]);
        assert_eq!(switch_multilevel_report(0x32), [0x26, 0x03, 0x32]);
        assert_eq!(scene_activation_set(1, 0), [0x2B, 0x01, 0x01, 0x00]);
        assert_eq!(meter_get(), [0x32, 0x01, 0x00]);
        assert_eq!(
            meter_report(0x01, &[0x21, 0x01]),
            [0x32, 0x02, 0x01, 0x21, 0x01]
        );
    }

    #[test]
    fn oversized_payloads_are_rejected() {
        let too_long = vec![0u8; FRAME_MAX_LENGTH];
        assert!(matches!(
            build_send_data(1, &too_long, 0, 1),
            Err(BuildError::DataTooLong(_))
        ));
    }
}
