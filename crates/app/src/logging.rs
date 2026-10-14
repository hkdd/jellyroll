//! tracing setup: journald when started by systemd, stderr otherwise.
//!
//! Profiling (spec 13): the frame loop and the network thread open
//! TRACE-level spans around each stage (`wait`, `net`, `update`, `video`,
//! `upload`, `draw`, `submit`, `swap`; `decode` for artwork). The log's
//! level filter is per layer, so with the usual `info` those spans are
//! disabled where they are written (a cached check, no allocation). Built
//! with `--features profile` and run with `--trace FILE`, a Chrome-trace
//! layer records them (and info-level events) to FILE.

use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Parses RUST_LOG, else the config's `log.level`, as `level` or
/// `target=level,...` (no regex engine; see docs/DECISIONS.md). Invalid
/// values fall back to `info`.
fn filter(config_level: &str) -> Targets {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<Targets>().ok())
        .or_else(|| config_level.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO))
}

/// Keeps the trace file open; dropping it writes the end of the file.
pub struct TraceGuard {
    #[cfg(feature = "profile")]
    _flush: tracing_chrome::FlushGuard,
}

#[cfg(feature = "profile")]
type TraceLayer = tracing_subscriber::filter::Filtered<
    tracing_chrome::ChromeLayer<tracing_subscriber::Registry>,
    tracing_subscriber::filter::FilterFn,
    tracing_subscriber::Registry,
>;
#[cfg(not(feature = "profile"))]
type TraceLayer = tracing_subscriber::layer::Identity;

/// What the trace records: jellyroll's spans, and what would be logged
/// anyway; not every dependency's trace output.
#[cfg(feature = "profile")]
fn traced(m: &tracing::Metadata<'_>) -> bool {
    (m.is_span() && m.target().starts_with("jellyroll")) || *m.level() <= tracing::Level::INFO
}

/// The Chrome-trace layer for `--trace FILE`.
#[cfg(feature = "profile")]
fn trace_layer(path: &std::path::Path) -> (TraceLayer, TraceGuard) {
    let (layer, flush) =
        tracing_chrome::ChromeLayerBuilder::new().file(path).include_args(true).build();
    let wanted =
        tracing_subscriber::filter::filter_fn(traced as fn(&tracing::Metadata<'_>) -> bool);
    (layer.with_filter(wanted), TraceGuard { _flush: flush })
}

/// Sets up logging; with `trace` (a `profile` build), also the frame
/// timeline. Keep the guard until exit.
pub fn init(config_level: &str, trace: Option<&std::path::Path>) -> Option<TraceGuard> {
    #[cfg(feature = "profile")]
    let (chrome, guard) = match trace.map(trace_layer) {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    #[cfg(not(feature = "profile"))]
    let (chrome, guard): (Option<TraceLayer>, Option<TraceGuard>) = {
        if trace.is_some() {
            eprintln!(
                "jellyroll: --trace needs a build with the profile feature \
                 (cargo build --release --features profile); ignored"
            );
        }
        (None, None)
    };
    let registry = tracing_subscriber::registry().with(chrome);
    // Use native journald fields (priorities, spans) only when stderr really
    // is the journal stream systemd set up for us.
    if stderr_is_journal()
        && let Ok(layer) = tracing_journald::layer()
    {
        let layer = layer.with_syslog_identifier("jellyroll".into());
        registry.with(layer.with_filter(filter(config_level))).init();
        return guard;
    }
    let fmt = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(false);
    registry.with(fmt.with_filter(filter(config_level))).init();
    guard
}

/// systemd sets JOURNAL_STREAM to the `device:inode` of the stream it
/// connected to stdout/stderr. The variable is inherited by child processes
/// whose stderr goes elsewhere (e.g. a CI runner that is itself a systemd
/// service), so it only counts when stderr's identity matches (per
/// systemd.exec(5)).
fn stderr_is_journal() -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(var) = std::env::var_os("JOURNAL_STREAM") else { return false };
    let Ok(meta) = std::fs::metadata("/proc/self/fd/2") else { return false };
    journal_stream_matches(&var.to_string_lossy(), meta.dev(), meta.ino())
}

fn journal_stream_matches(var: &str, dev: u64, ino: u64) -> bool {
    let Some((d, i)) = var.split_once(':') else { return false };
    d.parse() == Ok(dev) && i.parse() == Ok(ino)
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

#[cfg(test)]
mod tests {
    use super::journal_stream_matches;

    #[test]
    fn journal_stream_must_match_stderr() {
        assert!(journal_stream_matches("8:12345", 8, 12345));
        assert!(!journal_stream_matches("8:12345", 8, 99));
        assert!(!journal_stream_matches("9:12345", 8, 12345));
        assert!(!journal_stream_matches("garbage", 8, 12345));
        assert!(!journal_stream_matches("", 0, 0));
    }
}
