use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get_service;
use hodeco_contract::config::StaticAssetsConfig;
use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use tower::Service;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AssetsState {
    dir: PathBuf,
    index_name: String,
    index_file: PathBuf,
    spa_fallback: bool,
    precompressed: bool,
}

pub fn router(config: &StaticAssetsConfig) -> Router {
    let state = AssetsState {
        dir: config.dir.clone(),
        index_name: config.index.clone(),
        index_file: config.dir.join(&config.index),
        spa_fallback: config.spa_fallback,
        precompressed: config.precompressed,
    };

    let router = if config.spa_fallback {
        let index = serve_file(&state.index_file, config.precompressed);
        let root = index.clone();
        let fallback = tower::service_fn(move |request: Request| {
            let mut index = index.clone();
            async move {
                if is_navigation(&request) {
                    let response = index
                        .call(request)
                        .await
                        .unwrap_or_else(|never| match never {});
                    Ok::<_, Infallible>(response.map(Body::new))
                } else {
                    Ok::<_, Infallible>(StatusCode::NOT_FOUND.into_response())
                }
            }
        });

        Router::new()
            .route("/", get_service(root))
            .fallback_service(serve_dir(config).fallback(fallback))
    } else {
        Router::new().fallback_service(serve_dir(config))
    };

    let cache_state = state;
    router.layer(middleware::from_fn(move |request: Request, next: Next| {
        let state = cache_state.clone();
        async move { static_headers(state, request, next).await }
    }))
}

fn serve_file(path: &Path, precompressed: bool) -> ServeFile {
    let file = ServeFile::new(path);
    if precompressed {
        file.precompressed_br().precompressed_gzip()
    } else {
        file
    }
}

fn serve_dir(config: &StaticAssetsConfig) -> ServeDir {
    let dir = ServeDir::new(&config.dir);
    if config.precompressed {
        dir.precompressed_br().precompressed_gzip()
    } else {
        dir
    }
}

async fn static_headers(state: AssetsState, mut request: Request, next: Next) -> Response {
    let navigation = is_navigation(&request);
    let uri_path = request.uri().path().to_owned();

    // JSON assets (notably the Z-Wave and entity definitions) are meant to be
    // edited in place next to the WASM binary. tower-http's precompressed
    // variants are selected by `Accept-Encoding` and never compare the raw
    // file's mtime, so a stale `.gz`/`.br` sidecar would keep shadowing an
    // edited JSON file. Read JSON from the raw file instead; the outer
    // compression layer still compresses the response on the fly.
    if is_json_path(&uri_path) {
        request.headers_mut().remove(header::ACCEPT_ENCODING);
    }

    let mut response = next.run(request).await;

    response.headers_mut().insert(
        header::CACHE_CONTROL,
        if is_json_path(&uri_path) {
            // Runtime-editable definitions must be re-read whenever the app is
            // reloaded, even when the browser has a copy from an earlier run.
            HeaderValue::from_static("no-store")
        } else {
            HeaderValue::from_static("public, max-age=0, must-revalidate")
        },
    );
    if state.precompressed && !response.headers().contains_key(header::VARY) {
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }

    // ServeDir/ServeFile already set Last-Modified on file responses. Only
    // resolve the file again for 304 responses, where tower-http omits it.
    let needs_last_modified = !response.headers().contains_key(header::LAST_MODIFIED)
        && (response.status() == StatusCode::NOT_MODIFIED || response.status().is_success());
    if needs_last_modified
        && let Some(last_modified) = resolve_last_modified(&state, &uri_path, navigation).await
        && let Some(date) = http_date(last_modified)
        && let Ok(value) = HeaderValue::from_str(&date)
    {
        response.headers_mut().insert(header::LAST_MODIFIED, value);
    }

    response
}

async fn resolve_last_modified(
    state: &AssetsState,
    uri_path: &str,
    navigation: bool,
) -> Option<SystemTime> {
    let relative = decode_request_path(uri_path)?;
    let mut candidate = state.dir.join(relative);

    match tokio::fs::metadata(&candidate).await {
        Ok(metadata) if metadata.is_dir() => {
            candidate = candidate.join(&state.index_name);
            tokio::fs::metadata(&candidate).await.ok()?.modified().ok()
        }
        Ok(metadata) => metadata.modified().ok(),
        Err(_) if state.spa_fallback && navigation => tokio::fs::metadata(&state.index_file)
            .await
            .ok()?
            .modified()
            .ok(),
        Err(_) => None,
    }
}

fn decode_request_path(uri_path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(uri_path)
        .decode_utf8()
        .ok()?;
    let mut path = PathBuf::new();

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(path)
}

fn is_navigation(request: &Request) -> bool {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return false;
    }

    let path = percent_encoding::percent_decode_str(request.uri().path()).decode_utf8_lossy();
    if path == "/api" || path.starts_with("/api/") {
        return false;
    }
    let last_segment = path.rsplit('/').next().unwrap_or_default();
    !last_segment.contains('.')
}

fn is_json_path(uri_path: &str) -> bool {
    Path::new(uri_path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn http_date(time: SystemTime) -> Option<String> {
    let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(duration.as_secs()).ok()?;
    let time = chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, duration.subsec_nanos())?;
    Some(time.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}
