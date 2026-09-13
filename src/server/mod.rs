pub mod handlers;

use crate::metrics::Metrics;
use crate::store::Store;
use axum::http::HeaderValue;
use axum::{Router, routing::get, serve::Listener};
use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};

pub const DEFAULT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

pub struct AppState {
    pub store: Arc<Store>,
    pub metrics: Arc<Metrics>,
    pub default_value: Option<Bytes>,
    pub content_type: HeaderValue,
    pub started: Instant,
    pub keys: usize,
    pub arena_bytes: usize,
    pub source: String,
    pub format: &'static str,
    pub load_ms: f64,
    pub timing: bool,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/kv/{key}", get(handlers::kv_path))
        .route("/kv", get(handlers::kv_query))
        .route("/health", get(handlers::health))
        .route("/metrics", get(handlers::metrics))
        .route("/stats", get(handlers::stats))
        .with_state(state)
}

/// Wraps a `TcpListener` purely to set `TCP_NODELAY` on accepted sockets.
///
/// `axum::serve` accepts connections internally, so this is the only hook
/// available. Without it, Nagle's algorithm can add up to 40ms to small
/// responses — which is most of them here.
struct NoDelayListener(TcpListener);

impl Listener for NoDelayListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        // Per-call backoff, reset on every success, so an isolated error costs
        // nothing but a sustained one stops spinning.
        let mut backoff = Duration::from_millis(1);
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    return (stream, addr);
                }
                // Transient accept errors (a connection reset during handshake)
                // must not kill the server. But a persistent one — EMFILE above
                // all — leaves the socket readable, so returning straight to
                // `accept` spins at full CPU and starves the very tasks that
                // would close connections and release the descriptors. Yield
                // for a moment instead, growing to a ceiling.
                Err(_) => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(256));
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

/// Bind and serve until SIGTERM/SIGINT.
///
/// Callers must finish loading before calling this: an unbound port makes
/// orchestrators hold traffic back, which is better than binding early and
/// answering 503s.
pub async fn run(state: Arc<AppState>, bind: &str) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    eprintln!(
        "justkv listening on {addr} ({} keys, {} bytes)",
        state.keys, state.arena_bytes
    );
    axum::serve(NoDelayListener(listener), router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Bind to an ephemeral port and return the address alongside the server
/// future. Used by integration tests, which must not guess a free port.
pub async fn run_ephemeral(
    state: Arc<AppState>,
) -> anyhow::Result<(SocketAddr, impl std::future::Future<Output = ()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(state);
    let fut = async move {
        let _ = axum::serve(NoDelayListener(listener), app).await;
    };
    Ok((addr, fut))
}
