use bytes::Bytes;
use justkv::metrics::Metrics;
use justkv::server::{AppState, run_ephemeral};
use justkv::store::Store;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

struct Resp {
    status: u16,
    headers: String,
    body: String,
}

/// Minimal HTTP/1.1 client. Avoids adding a client dependency for what is
/// a handful of plaintext localhost requests.
async fn get(addr: SocketAddr, target: &str) -> Resp {
    request(addr, "GET", target).await
}

async fn request(addr: SocketAddr, method: &str, target: &str) -> Resp {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("{method} {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Resp {
        status,
        headers: head.to_lowercase(),
        body: body.to_string(),
    }
}

fn state_with(default_value: Option<&str>) -> Arc<AppState> {
    // keys: "a"->"1", "b"->"2", "a/b"->"slash", ""->"empty"
    let arena = b"a1b2a/bslashempty".to_vec();
    let entries = vec![
        justkv::store::Entry {
            k_off: 0,
            k_len: 1,
            v_off: 1,
            v_len: 1,
        },
        justkv::store::Entry {
            k_off: 2,
            k_len: 1,
            v_off: 3,
            v_len: 1,
        },
        justkv::store::Entry {
            k_off: 4,
            k_len: 3,
            v_off: 7,
            v_len: 5,
        },
        justkv::store::Entry {
            k_off: 0,
            k_len: 0,
            v_off: 12,
            v_len: 5,
        },
    ];
    let store = Store::from_parts(arena, entries);
    Arc::new(AppState {
        keys: store.len(),
        arena_bytes: store.arena_len(),
        source: "test".into(),
        format: "csv",
        load_ms: 0.0,
        store: Arc::new(store),
        metrics: Arc::new(Metrics::new()),
        default_value: default_value.map(|d| Bytes::from(d.to_string().into_bytes())),
        content_type: axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
        started: Instant::now(),
        timing: true,
    })
}

async fn boot(state: Arc<AppState>) -> SocketAddr {
    let (addr, fut) = run_ephemeral(state).await.unwrap();
    tokio::spawn(fut);
    addr
}

#[tokio::test]
async fn hit_returns_value_with_text_plain() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/kv/a").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "1");
    assert!(
        r.headers
            .contains("content-type: text/plain; charset=utf-8"),
        "{}",
        r.headers
    );
    assert!(r.headers.contains("content-length: 1"), "{}", r.headers);
    assert!(!r.headers.contains("x-justkv-default"));
}

#[tokio::test]
async fn miss_without_default_returns_404_and_empty_body() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/kv/zzz").await;
    assert_eq!(r.status, 404);
    assert!(r.body.is_empty(), "body was {:?}", r.body);
}

#[tokio::test]
async fn miss_with_default_returns_200_and_marker_header() {
    let addr = boot(state_with(Some("NA"))).await;
    let r = get(addr, "/kv/zzz").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "NA");
    assert!(r.headers.contains("x-justkv-default: 1"), "{}", r.headers);
}

#[tokio::test]
async fn default_served_response_counts_as_miss_in_metrics() {
    let addr = boot(state_with(Some("NA"))).await;
    get(addr, "/kv/zzz").await;
    let m = get(addr, "/metrics").await;
    assert!(m.body.contains("justkv_hits_total 0\n"), "{}", m.body);
    assert!(m.body.contains("justkv_misses_total 1\n"), "{}", m.body);
    assert!(
        m.body.contains("justkv_defaults_served_total 1\n"),
        "{}",
        m.body
    );
}

#[tokio::test]
async fn key_containing_slash_works_via_percent_encoded_path() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/kv/a%2Fb").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "slash");
}

#[tokio::test]
async fn key_containing_slash_works_via_query_form() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/kv?k=a%2Fb").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "slash");
}

#[tokio::test]
async fn empty_key_is_reachable_only_via_query_form() {
    let addr = boot(state_with(None)).await;
    assert_eq!(get(addr, "/kv?k=").await.body, "empty");
    // The path form has no segment to match, so the route does not fire.
    assert_eq!(get(addr, "/kv/").await.status, 404);
}

#[tokio::test]
async fn query_form_without_k_is_a_400() {
    let addr = boot(state_with(None)).await;
    assert_eq!(get(addr, "/kv?j=1").await.status, 400);
}

#[tokio::test]
async fn health_returns_ok() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/health").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "ok");
}

#[tokio::test]
async fn stats_reports_dataset_shape() {
    let addr = boot(state_with(None)).await;
    let r = get(addr, "/stats").await;
    assert_eq!(r.status, 200);
    let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["keys"], 4);
    assert_eq!(v["arena_bytes"], 17);
    assert_eq!(v["format"], "csv");
    assert_eq!(v["default_value_configured"], false);
}

#[tokio::test]
async fn metrics_accumulate_hits_and_bytes() {
    let addr = boot(state_with(None)).await;
    get(addr, "/kv/a").await;
    get(addr, "/kv/b").await;
    let m = get(addr, "/metrics").await;
    assert!(m.body.contains("justkv_requests_total 2\n"), "{}", m.body);
    assert!(m.body.contains("justkv_hits_total 2\n"), "{}", m.body);
    assert!(
        m.body.contains("justkv_response_bytes_total 2\n"),
        "{}",
        m.body
    );
    assert!(m.body.contains("justkv_keys 4\n"), "{}", m.body);
}

#[tokio::test]
async fn head_request_does_not_count_bytes_it_never_sends() {
    // axum routes HEAD to the GET handler and strips the body afterwards, so
    // the handler sees a full-size value while the client receives none.
    // justkv_response_bytes_total advertises bytes written to clients.
    let addr = boot(state_with(None)).await;
    let h = request(addr, "HEAD", "/kv/a").await;
    assert_eq!(h.status, 200);
    assert!(h.body.is_empty(), "HEAD returned a body: {:?}", h.body);

    let m = get(addr, "/metrics").await;
    assert!(
        m.body.contains("justkv_response_bytes_total 0\n"),
        "HEAD inflated byte accounting:\n{}",
        m.body
    );
    // The lookup itself still counts as a request and a hit.
    assert!(m.body.contains("justkv_hits_total 1\n"), "{}", m.body);
}
