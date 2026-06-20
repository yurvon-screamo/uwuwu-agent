//! Process signal handling for the proxy server's graceful shutdown.
//!
//! [`shutdown_signal`] resolves on the first signal received (Ctrl+C on
//! every platform; SIGTERM on unix) so the §12 drain sequence in
//! `serve_with_shutdown` stops accepting new connections in order.

/// Wait for Ctrl+C (all platforms) or SIGTERM (unix). Returns on the first
/// signal so the server stops accepting new connections and the §12 drain
/// sequence begins.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Ctrl+C received, initiating graceful shutdown"),
        _ = terminate => tracing::info!("SIGTERM received, initiating graceful shutdown"),
    }
}
