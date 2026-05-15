//! SIGHUP-triggered hot-reload of the abuse deny-lists.
//!
//! When `abuse.hot_reload_enabled`, an operator edits the deny-list YAML (or
//! the URL patterns) and sends SIGHUP; the new rules are validated and
//! swapped in atomically, no restart. A malformed edit is rejected and the
//! running rules stay. When the feature is off no handler is installed, so
//! SIGHUP keeps its default disposition (process termination) — the caller's
//! config documents that.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::AppConfig;
use crate::security::AbuseGuard;

/// Spawns the SIGHUP reload loop. Returns `None` when the feature is off or
/// the platform has no SIGHUP; otherwise the join handle so `main` drives
/// graceful shutdown alongside the other background workers.
pub fn spawn(
    abuse: Arc<AbuseGuard>,
    cfg: Arc<AppConfig>,
    shutdown: CancellationToken,
) -> Option<JoinHandle<()>> {
    if !cfg.abuse.hot_reload_enabled {
        return None;
    }
    #[cfg(unix)]
    {
        Some(tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut hup = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(err) => {
                    tracing::error!(?err, "SIGHUP handler install failed; abuse hot-reload off");
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = hup.recv() => match abuse.reload(&cfg.abuse) {
                        Ok(()) => tracing::info!("abuse deny-lists reloaded on SIGHUP"),
                        Err(err) => tracing::error!(
                            ?err,
                            "abuse deny-list reload rejected; keeping previous rules"
                        ),
                    },
                }
            }
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = (abuse, shutdown);
        None
    }
}
