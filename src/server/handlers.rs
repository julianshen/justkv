use crate::metrics::Outcome;
use crate::server::AppState;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use std::sync::Arc;
use std::time::Instant;

/// Pull `k` out of the raw query string and percent-decode it to bytes.
///
/// Done by hand rather than with a typed extractor because keys are bytes,
/// not necessarily UTF-8, and `serde` would force a lossy `String` on us.
pub(crate) fn key_from_query(q: Option<&str>) -> Option<Vec<u8>> {
    let q = q?;
    for pair in q.split('&') {
        let (name, value) = match pair.split_once('=') {
            Some(p) => p,
            None => (pair, ""),
        };
        if name == "k" {
            // `+` means space in a query string; percent-encoding handles the rest.
            let plus_decoded: Vec<u8> = value
                .as_bytes()
                .iter()
                .map(|&b| if b == b'+' { b' ' } else { b })
                .collect();
            return Some(percent_encoding::percent_decode(&plus_decoded).collect::<Vec<u8>>());
        }
    }
    None
}

pub async fn kv_path(State(s): State<Arc<AppState>>, Path(key): Path<String>) -> Response {
    lookup(&s, key.as_bytes())
}

pub async fn kv_query(State(s): State<Arc<AppState>>, RawQuery(q): RawQuery) -> Response {
    match key_from_query(q.as_deref()) {
        Some(key) => lookup(&s, &key),
        // No `k` at all is a malformed request, distinct from `k=` (empty key).
        None => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("missing required query parameter: k\n"))
            .unwrap(),
    }
}

fn lookup(s: &AppState, key: &[u8]) -> Response {
    let start = s.timing.then(Instant::now);

    let (outcome, body, status, is_default) = match s.store.get(key) {
        Some(v) => (Outcome::Hit, Some(v), StatusCode::OK, false),
        None => match &s.default_value {
            Some(d) => (Outcome::Default, Some(d.clone()), StatusCode::OK, true),
            None => (Outcome::Miss, None, StatusCode::NOT_FOUND, false),
        },
    };

    let len = body.as_ref().map_or(0, |b| b.len());
    s.metrics.record(outcome, len, start.map(|t| t.elapsed()));

    let mut b = Response::builder().status(status);
    if let Some(v) = body {
        b = b.header(header::CONTENT_TYPE, s.content_type.clone());
        if is_default {
            // Lets clients and proxies distinguish a real hit from a substitute.
            b = b.header("x-justkv-default", "1");
        }
        // axum sets Content-Length from a sized body.
        return b.body(Body::from(v)).unwrap();
    }
    b.body(Body::empty()).unwrap()
}

pub async fn health() -> &'static str {
    "ok"
}

pub async fn metrics(State(s): State<Arc<AppState>>) -> Response {
    let text = s.metrics.render(s.keys, s.arena_bytes, s.started.elapsed());
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )
        .body(Body::from(text))
        .unwrap()
}

pub async fn stats(State(s): State<Arc<AppState>>) -> Response {
    let body = serde_json::json!({
        "keys": s.keys,
        "arena_bytes": s.arena_bytes,
        "source": s.source,
        "format": s.format,
        "load_ms": s.load_ms,
        "uptime_seconds": s.started.elapsed().as_secs(),
        "default_value_configured": s.default_value.is_some(),
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_k_parameter_from_raw_query() {
        assert_eq!(key_from_query(Some("k=abc")).as_deref(), Some(&b"abc"[..]));
        assert_eq!(
            key_from_query(Some("x=1&k=abc&y=2")).as_deref(),
            Some(&b"abc"[..])
        );
    }

    #[test]
    fn empty_k_parameter_is_the_empty_key_not_absent() {
        assert_eq!(key_from_query(Some("k=")).as_deref(), Some(&b""[..]));
    }

    #[test]
    fn missing_k_parameter_yields_none() {
        assert!(key_from_query(Some("j=abc")).is_none());
        assert!(key_from_query(None).is_none());
    }

    #[test]
    fn percent_decodes_and_allows_non_utf8_bytes() {
        assert_eq!(
            key_from_query(Some("k=a%2Fb")).as_deref(),
            Some(&b"a/b"[..])
        );
        assert_eq!(
            key_from_query(Some("k=%FF%FE")).as_deref(),
            Some(&b"\xff\xfe"[..])
        );
    }

    #[test]
    fn plus_is_decoded_as_space() {
        assert_eq!(key_from_query(Some("k=a+b")).as_deref(), Some(&b"a b"[..]));
    }
}
