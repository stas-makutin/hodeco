use super::ServiceError;
use super::catalog::{
    SERIAL_PARAM_BAUD_RATE, SERIAL_PARAM_DATA_BITS, SERIAL_PARAM_PARITY, SERIAL_PARAM_READ_TIMEOUT,
    SERIAL_PARAM_STOP_BITS, SERIAL_PARAM_WRITE_TIMEOUT,
};
use hodeco_contract::domain::{ParamError, ParamValues, param_duration, param_str, param_u64};
use serde_json::json;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, StopBits};

pub const DEFAULT_BAUD_RATE: u32 = 115_200;

pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

pub type Port = Box<dyn AsyncReadWrite>;

#[derive(Debug, Clone, PartialEq)]
pub struct SerialSettings {
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub read_timeout: Option<Duration>,
    pub write_timeout: Option<Duration>,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud_rate: DEFAULT_BAUD_RATE,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            read_timeout: None,
            write_timeout: None,
        }
    }
}

impl SerialSettings {
    pub fn from_params(params: &ParamValues) -> Result<Self, ServiceError> {
        let baud_rate = u32::try_from(
            param_u64(params, SERIAL_PARAM_BAUD_RATE).unwrap_or(u64::from(DEFAULT_BAUD_RATE)),
        )
        .map_err(|_| {
            invalid_param(
                SERIAL_PARAM_BAUD_RATE,
                params
                    .get(SERIAL_PARAM_BAUD_RATE)
                    .cloned()
                    .unwrap_or_default(),
            )
        })?;

        let data_bits = match param_str(params, SERIAL_PARAM_DATA_BITS).unwrap_or("8") {
            "5" => DataBits::Five,
            "6" => DataBits::Six,
            "7" => DataBits::Seven,
            "8" => DataBits::Eight,
            value => return Err(invalid_param(SERIAL_PARAM_DATA_BITS, json!(value))),
        };

        let parity = match param_str(params, SERIAL_PARAM_PARITY).unwrap_or("none") {
            "none" => Parity::None,
            "odd" => Parity::Odd,
            "even" => Parity::Even,
            value => return Err(invalid_param(SERIAL_PARAM_PARITY, json!(value))),
        };

        let stop_bits = match param_str(params, SERIAL_PARAM_STOP_BITS).unwrap_or("1") {
            "1" => StopBits::One,
            "2" => StopBits::Two,
            value => return Err(invalid_param(SERIAL_PARAM_STOP_BITS, json!(value))),
        };

        Ok(Self {
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            read_timeout: param_duration(params, SERIAL_PARAM_READ_TIMEOUT)
                .filter(|timeout| !timeout.is_zero()),
            write_timeout: param_duration(params, SERIAL_PARAM_WRITE_TIMEOUT)
                .filter(|timeout| !timeout.is_zero()),
        })
    }
}

fn invalid_param(name: &str, value: serde_json::Value) -> ServiceError {
    ServiceError::InvalidParams(ParamError::Invalid {
        name: name.to_string(),
        value,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDescriptor {
    pub name: String,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    pub serial_number: Option<String>,
}

pub fn available_ports() -> io::Result<Vec<PortDescriptor>> {
    let ports = tokio_serial::available_ports().map_err(io::Error::other)?;
    Ok(ports
        .into_iter()
        .map(|port| {
            let (product, manufacturer, serial_number) = match port.port_type {
                tokio_serial::SerialPortType::UsbPort(info) => {
                    (info.product, info.manufacturer, info.serial_number)
                }
                _ => (None, None, None),
            };
            PortDescriptor {
                name: port.port_name,
                product,
                manufacturer,
                serial_number,
            }
        })
        .collect())
}

pub trait PortEnumerator: Send + Sync {
    fn ports(&self) -> io::Result<Vec<PortDescriptor>>;
}

pub struct SystemPortEnumerator;

impl PortEnumerator for SystemPortEnumerator {
    fn ports(&self) -> io::Result<Vec<PortDescriptor>> {
        available_ports()
    }
}

pub trait PortOpener: Send + Sync {
    fn open(
        &self,
        entry: String,
        settings: SerialSettings,
    ) -> Pin<Box<dyn Future<Output = io::Result<Port>> + Send + 'static>>;
}

pub struct SystemPortOpener;

impl PortOpener for SystemPortOpener {
    fn open(
        &self,
        entry: String,
        settings: SerialSettings,
    ) -> Pin<Box<dyn Future<Output = io::Result<Port>> + Send + 'static>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                open_serial(&entry, &settings).map(|stream| Box::new(stream) as Port)
            })
            .await
            .map_err(io::Error::other)?
        })
    }
}

pub fn open_serial(
    entry: &str,
    settings: &SerialSettings,
) -> io::Result<tokio_serial::SerialStream> {
    let mut builder = tokio_serial::new(entry, settings.baud_rate)
        .data_bits(settings.data_bits)
        .parity(settings.parity)
        .stop_bits(settings.stop_bits)
        .flow_control(FlowControl::None);

    if let Some(timeout) = settings.read_timeout {
        builder = builder.timeout(timeout);
    }

    builder.open_native_async().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_settings() {
        let settings = SerialSettings::from_params(&ParamValues::new()).unwrap();
        assert_eq!(settings.baud_rate, DEFAULT_BAUD_RATE);
        assert_eq!(settings.data_bits, DataBits::Eight);
        assert_eq!(settings.parity, Parity::None);
        assert_eq!(settings.stop_bits, StopBits::One);
        assert!(settings.read_timeout.is_none());
        assert!(settings.write_timeout.is_none());
    }

    #[test]
    fn parses_custom_settings() {
        let params = ParamValues::from([
            ("baudRate".to_string(), json!(57_600)),
            ("dataBits".to_string(), json!("7")),
            ("parity".to_string(), json!("even")),
            ("stopBits".to_string(), json!("2")),
            ("readTimeout".to_string(), json!("250 ms")),
        ]);
        let settings = SerialSettings::from_params(&params).unwrap();
        assert_eq!(settings.baud_rate, 57_600);
        assert_eq!(settings.data_bits, DataBits::Seven);
        assert_eq!(settings.parity, Parity::Even);
        assert_eq!(settings.stop_bits, StopBits::Two);
        assert_eq!(settings.read_timeout, Some(Duration::from_millis(250)));
    }

    #[test]
    fn rejects_invalid_settings() {
        let params = ParamValues::from([("dataBits".to_string(), json!("9"))]);
        assert!(SerialSettings::from_params(&params).is_err());
    }
}
