//! tracing setup: journald when started by systemd, stderr otherwise.

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

pub fn init(config_level: &str) {
    let registry = tracing_subscriber::registry().with(filter(config_level));
    // Use native journald fields (priorities, spans) only when stderr really
    // is the journal stream systemd set up for us.
    if stderr_is_journal()
        && let Ok(layer) = tracing_journald::layer()
    {
        registry.with(layer.with_syslog_identifier("jellyroll".into())).init();
        return;
    }
    registry
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .init();
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
