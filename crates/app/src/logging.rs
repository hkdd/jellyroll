//! tracing setup: journald when started by systemd, stderr otherwise.

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Parses RUST_LOG as `level` or `target=level,...` (no regex engine; see
/// docs/DECISIONS.md). Invalid values fall back to `info`.
fn filter() -> Targets {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO))
}

pub fn init() {
    let registry = tracing_subscriber::registry().with(filter());
    // systemd sets JOURNAL_STREAM when stdout/stderr go to the journal; use
    // native journald fields then, so priorities and spans are preserved.
    if std::env::var_os("JOURNAL_STREAM").is_some()
        && let Ok(layer) = tracing_journald::layer()
    {
        registry.with(layer.with_syslog_identifier("jf-htpc".into())).init();
        return;
    }
    registry
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .init();
}

/// Logs panics through tracing (journald) before the default hook runs.
/// Release builds use `panic = "abort"`, so this is the last output.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic: {info}\n{bt}");
        default(info);
    }));
}
