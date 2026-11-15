//! Pure protocol decoding and static reference data for the UI.
//!
//! This crate intentionally has no `egui` dependency. It produces semantic
//! decode results; the UI decides how fields are coloured and laid out.

pub mod builder;
pub mod definitions;
pub mod entities;
pub mod filter;
pub mod hex;
pub mod parsed;
pub mod zwave;

/// Maximum payload bytes decoded for compact summaries.
pub const SUMMARY_DECODE_LIMIT: usize = 4 * 1024;
/// Maximum characters kept in a compact message summary.
pub const SUMMARY_MAX_CHARS: usize = 160;

pub use builder::{
    BROADCAST_TX_OPTIONS, BuildError, TX_OPTION_ACK, TX_OPTION_AUTO_ROUTE, TX_OPTION_EXPLORE,
    TX_OPTION_LOW_POWER, TX_OPTION_NO_ROUTE, basic_get, basic_report, basic_set, build_broadcast,
    build_get_version, build_request, build_response, build_send_data, build_send_data_multi,
    complete_raw_frame, meter_get, meter_report, scene_activation_set, switch_binary_get,
    switch_binary_report, switch_binary_set, switch_multilevel_get, switch_multilevel_report,
    switch_multilevel_set,
};

pub use hex::{DumpRow, HexError, dump, parse_hex, spaced_hex};
pub use parsed::{DecodeResult, DecodedField, DecodedFrame, FieldKind, FrameKind};
pub use zwave::{build_frame, checksum};
