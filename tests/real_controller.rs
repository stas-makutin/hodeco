//! Manual integration test against a real Z-Wave serial controller.
//!
//! Run with:
//! `cargo test --test real_controller -- --ignored --nocapture`
//!
//! Set `HODECO_SERIAL_PORT` to override the default `COM4`.

use hodeco::services::serial::{SerialSettings, open_serial};
use hodeco::services::zwave::{
    FRAME_ACK, FRAME_CAN, FRAME_NAK, FRAME_SOF, FrameValidation, ZW_VERSION, unpack_response,
    validate_data_frame, version_request,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

#[tokio::test]
#[ignore = "requires a real Z-Wave controller; set HODECO_SERIAL_PORT (default COM4)"]
async fn real_controller_responds_to_version_request() {
    let port_name = std::env::var("HODECO_SERIAL_PORT").unwrap_or_else(|_| "COM4".to_string());
    let mut port = open_serial(&port_name, &SerialSettings::default())
        .unwrap_or_else(|error| panic!("unable to open '{port_name}': {error}"));

    port.write_all(&version_request())
        .await
        .unwrap_or_else(|error| panic!("unable to write to '{port_name}': {error}"));
    port.flush().await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut buffer = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "no ZW_VERSION response from '{port_name}'; received {buffer:02X?}"
        );

        let mut chunk = [0_u8; 256];
        let count = timeout(remaining, port.read(&mut chunk))
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for '{port_name}'; received {buffer:02X?}")
            })
            .unwrap_or_else(|error| panic!("unable to read from '{port_name}': {error}"));
        assert!(count > 0, "serial port '{port_name}' closed unexpectedly");
        buffer.extend_from_slice(&chunk[..count]);

        while let Some(first) = buffer.first().copied() {
            match first {
                FRAME_ACK | FRAME_NAK | FRAME_CAN => {
                    buffer.remove(0);
                }
                FRAME_SOF => match validate_data_frame(&buffer) {
                    (FrameValidation::Ok, consumed) => {
                        let frame = buffer[..consumed].to_vec();
                        buffer.drain(..consumed);

                        if let Some(body) = unpack_response(&frame)
                            && body.first() == Some(&ZW_VERSION)
                        {
                            assert!(
                                body.len() > 1,
                                "unexpected ZW_VERSION response body: {body:02X?}"
                            );
                            return;
                        }
                    }
                    (FrameValidation::Incomplete, _) => break,
                    (_, consumed) => {
                        buffer.drain(..consumed.max(1).min(buffer.len()));
                    }
                },
                _ => {
                    buffer.remove(0);
                }
            }
        }
    }
}
