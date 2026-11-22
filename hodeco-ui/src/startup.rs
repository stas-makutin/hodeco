//! Native command-line and browser-query startup configuration.

use crate::client::endpoint::{self, DEFAULT_NATIVE_ENDPOINT};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
    pub endpoint: String,
    pub demo: bool,
}

impl Startup {
    pub fn native(endpoint: impl Into<String>, demo: bool) -> Result<Self, String> {
        let endpoint = endpoint::normalize(&endpoint.into())
            .map_err(|error| format!("invalid --server endpoint: {error}"))?;
        Ok(Self { endpoint, demo })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
    Run(Startup),
    Help,
}

pub fn help_text() -> String {
    format!(
        "\
hodeco-ui - admin and diagnostics frontend for the hodeco gateway

USAGE:
    hodeco-ui [OPTIONS]

OPTIONS:
    --server <URL>    Gateway base URL or WebSocket URL
                      (default: {DEFAULT_NATIVE_ENDPOINT})
    --demo            Run with bundled fixture data; no network connection
    -h, --help        Print this help

Web:
    ?server=<URL>     Override the gateway endpoint
    ?demo=1           Run with bundled fixture data
"
    )
}

pub fn from_native_args<I, S>(args: I) -> Result<StartupAction, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut endpoint = DEFAULT_NATIVE_ENDPOINT.to_string();
    let mut demo = false;
    let mut args = args.into_iter().map(Into::into).peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--server" => {
                endpoint = args
                    .next()
                    .ok_or_else(|| "--server expects a URL".to_string())?;
            }
            "--demo" => demo = true,
            "-h" | "--help" => return Ok(StartupAction::Help),
            other if other.starts_with("--server=") => {
                endpoint = other["--server=".len()..].to_string();
            }
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }

    Startup::native(endpoint, demo).map(StartupAction::Run)
}

/// Parses startup data from a browser location. Split out from the DOM access so
/// it can be unit-tested on every platform.
pub fn from_web_location(href: &str) -> Result<Startup, String> {
    let location = Url::parse(href).map_err(|error| format!("invalid page URL: {error}"))?;
    let host = location
        .host_str()
        .ok_or_else(|| "page URL has no host".to_string())?;
    let host = match location.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let mut endpoint = endpoint::web_default(location.scheme(), &host)
        .map_err(|error| format!("cannot derive a WebSocket endpoint: {error}"))?;
    let mut demo = false;

    for (key, value) in location.query_pairs() {
        match key.as_ref() {
            "server" if !value.trim().is_empty() => {
                endpoint = endpoint::web_override(&location, &value)
                    .map_err(|error| format!("invalid ?server= endpoint: {error}"))?;
            }
            "demo" => {
                demo = value.is_empty()
                    || matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    );
            }
            _ => {}
        }
    }

    Ok(Startup { endpoint, demo })
}

#[cfg(target_arch = "wasm32")]
pub fn from_web() -> Result<Startup, String> {
    let location = web_sys::window()
        .and_then(|window| window.location().href().ok())
        .ok_or_else(|| "cannot read window.location".to_string())?;
    from_web_location(&location)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_defaults_and_overrides() {
        let action = from_native_args(Vec::<String>::new()).unwrap();
        assert_eq!(
            action,
            StartupAction::Run(Startup {
                endpoint: DEFAULT_NATIVE_ENDPOINT.to_string(),
                demo: false,
            })
        );

        let action = from_native_args(["--server", "http://127.0.0.1:9000", "--demo"]).unwrap();
        assert_eq!(
            action,
            StartupAction::Run(Startup {
                endpoint: "ws://127.0.0.1:9000/api/ws".to_string(),
                demo: true,
            })
        );

        let action = from_native_args(["--server=wss://example.com/api/ws"]).unwrap();
        assert_eq!(
            action,
            StartupAction::Run(Startup {
                endpoint: "wss://example.com/api/ws".to_string(),
                demo: false,
            })
        );
    }

    #[test]
    fn rejects_unknown_native_arguments() {
        assert!(from_native_args(["--wat"]).is_err());
        assert!(from_native_args(["--server"]).is_err());
        assert_eq!(from_native_args(["--help"]).unwrap(), StartupAction::Help);
    }

    #[test]
    fn parses_web_default_and_query_overrides() {
        let startup = from_web_location("http://127.0.0.1:8080/index.html").unwrap();
        assert_eq!(startup.endpoint, "ws://127.0.0.1:8080/api/ws");
        assert!(!startup.demo);

        let startup =
            from_web_location("https://gateway.example/ui/?server=192.168.1.5:8180&demo=1")
                .unwrap();
        assert_eq!(startup.endpoint, "ws://192.168.1.5:8180/api/ws");
        assert!(startup.demo);

        let startup =
            from_web_location("http://gateway.example/?server=%2Fprefix%2F&demo=true").unwrap();
        assert_eq!(startup.endpoint, "ws://gateway.example/prefix/api/ws");
        assert!(startup.demo);
    }
}
