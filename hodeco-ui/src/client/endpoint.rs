//! WebSocket endpoint normalization.

use thiserror::Error;
use url::Url;

pub const DEFAULT_NATIVE_ENDPOINT: &str = "ws://127.0.0.1:8180/api/ws";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EndpointError {
    #[error("endpoint is empty")]
    Empty,
    #[error("invalid endpoint '{value}': {reason}")]
    Invalid { value: String, reason: String },
    #[error("unsupported endpoint scheme '{scheme}'; use ws, wss, http, or https")]
    UnsupportedScheme { scheme: String },
    #[error("endpoint has no host")]
    MissingHost,
}

/// Accepts an HTTP(S) base URL, a WS(S) URL, or a bare `host:port` and returns
/// the canonical WebSocket URL. A missing `/api/ws` suffix is appended to any
/// base path, which also supports reverse-proxy prefixes.
pub fn normalize(input: &str) -> Result<String, EndpointError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::Empty);
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("ws://{trimmed}")
    };

    let mut url = Url::parse(&candidate).map_err(|error| EndpointError::Invalid {
        value: input.to_string(),
        reason: error.to_string(),
    })?;

    let scheme = match url.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        scheme => {
            return Err(EndpointError::UnsupportedScheme {
                scheme: scheme.to_string(),
            });
        }
    };

    if url.host_str().is_none() {
        return Err(EndpointError::MissingHost);
    }

    url.set_query(None);
    url.set_fragment(None);

    let mut path = url.path().trim_end_matches('/').to_string();
    if !path.ends_with("/api/ws") {
        path.push_str("/api/ws");
    }
    url.set_path(&path);
    url.set_scheme(scheme)
        .map_err(|()| EndpointError::Invalid {
            value: input.to_string(),
            reason: format!("cannot use the {scheme} scheme"),
        })?;

    Ok(url.to_string())
}

/// Builds the browser default: the same host as the page, with `/api/ws`.
pub fn web_default(protocol: &str, host: &str) -> Result<String, EndpointError> {
    if host.is_empty() {
        return Err(EndpointError::MissingHost);
    }
    let scheme = match protocol.trim_end_matches(':') {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        other => {
            return Err(EndpointError::UnsupportedScheme {
                scheme: other.to_string(),
            });
        }
    };
    normalize(&format!("{scheme}://{host}/api/ws"))
}

/// Resolves the `?gateway_url=` web override (the legacy `?server=` key is
/// handled by the caller). A leading slash is treated as a path on the current
/// origin; everything else is normalized as a full endpoint.
pub fn web_override(location: &Url, value: &str) -> Result<String, EndpointError> {
    let value = value.trim();
    if value.starts_with('/') {
        let host = location.host_str().ok_or(EndpointError::MissingHost)?;
        let port = location
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let scheme = if location.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        normalize(&format!("{scheme}://{host}{port}{value}"))
    } else {
        normalize(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_http_base_urls() {
        assert_eq!(
            normalize("http://127.0.0.1:8180").unwrap(),
            "ws://127.0.0.1:8180/api/ws"
        );
        assert_eq!(
            normalize("https://gateway.example").unwrap(),
            "wss://gateway.example/api/ws"
        );
        assert_eq!(
            normalize("http://gateway.example/prefix/").unwrap(),
            "ws://gateway.example/prefix/api/ws"
        );
    }

    #[test]
    fn keeps_websocket_urls_and_api_paths() {
        assert_eq!(
            normalize("ws://127.0.0.1:8180/api/ws").unwrap(),
            "ws://127.0.0.1:8180/api/ws"
        );
        assert_eq!(
            normalize("wss://example.com/ws/").unwrap(),
            "wss://example.com/ws/api/ws"
        );
        assert_eq!(
            normalize("ws://example.com/api/ws/").unwrap(),
            "ws://example.com/api/ws"
        );
    }

    #[test]
    fn accepts_bare_hosts() {
        assert_eq!(
            normalize("127.0.0.1:8180").unwrap(),
            "ws://127.0.0.1:8180/api/ws"
        );
        assert_eq!(
            normalize("gateway.local").unwrap(),
            "ws://gateway.local/api/ws"
        );
    }

    #[test]
    fn ignores_query_and_fragment() {
        assert_eq!(
            normalize("http://example.com/base?token=1#x").unwrap(),
            "ws://example.com/base/api/ws"
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(normalize("   ").unwrap_err(), EndpointError::Empty);
        assert!(matches!(
            normalize("ftp://example.com"),
            Err(EndpointError::UnsupportedScheme { .. })
        ));
        assert!(matches!(
            normalize("http://"),
            Err(EndpointError::Invalid { .. })
        ));
    }

    #[test]
    fn web_defaults_follow_the_page_scheme() {
        assert_eq!(
            web_default("http:", "127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/api/ws"
        );
        assert_eq!(
            web_default("https:", "gateway.example").unwrap(),
            "wss://gateway.example/api/ws"
        );
    }

    #[test]
    fn web_override_can_use_the_current_origin() {
        let location = Url::parse("http://127.0.0.1:8080/index.html").unwrap();
        assert_eq!(
            web_override(&location, "/gateway").unwrap(),
            "ws://127.0.0.1:8080/gateway/api/ws"
        );
        assert_eq!(
            web_override(&location, "192.168.1.10:8180").unwrap(),
            "ws://192.168.1.10:8180/api/ws"
        );
    }
}
