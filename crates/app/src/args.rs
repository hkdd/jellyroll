//! Hand-rolled CLI parsing (no extra dependency for a dozen flags).

use std::time::Duration;

pub const USAGE: &str = "\
jellyroll: boot-to-app Jellyfin HTPC client

USAGE: jellyroll [OPTIONS]

  --config PATH          Config file (default: ~/.config/jellyroll/config.toml)
  --dev                  Run in a desktop window (Wayland/X11/cage) instead of KMSDRM
  --hud                  Show the perf HUD at startup (toggle: F3, or Info x3 later)
  --allow-exit           Let Esc/Back on the top level quit (always on with --dev)
  --double-buffer        Request double instead of triple buffering (KMSDRM)
  --no-vsync             Disable vsync (benchmarking only)
  --bench-pacing SECS    Animate continuously for SECS, print frame stats, exit
                         non-zero if any budget is exceeded
  --bench-idle SECS      Sit on a static screen for SECS, verify no redraws and
                         < 1% CPU, exit non-zero otherwise
  --print-diagnostics    Print SDL/GL/display information and exit
  -h, --help             Show this help
";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub config: Option<std::path::PathBuf>,
    pub dev: bool,
    pub hud: bool,
    pub allow_exit: bool,
    pub double_buffer: bool,
    pub no_vsync: bool,
    pub bench: Option<Bench>,
    pub print_diagnostics: bool,
    pub help: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Bench {
    Pacing(Duration),
    Idle(Duration),
}

fn secs(v: Option<String>, flag: &str) -> Result<Duration, String> {
    let v = v.ok_or_else(|| format!("{flag} needs a value in seconds"))?;
    let s: f64 = v.parse().map_err(|_| format!("{flag}: invalid number '{v}'"))?;
    if !(s > 0.0 && s.is_finite()) {
        return Err(format!("{flag}: must be > 0"));
    }
    Ok(Duration::from_secs_f64(s))
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => {
                o.config = Some(it.next().ok_or("--config needs a path")?.into());
            }
            "--dev" => o.dev = true,
            "--hud" => o.hud = true,
            "--allow-exit" => o.allow_exit = true,
            "--double-buffer" => o.double_buffer = true,
            "--no-vsync" => o.no_vsync = true,
            "--bench-pacing" => o.bench = Some(Bench::Pacing(secs(it.next(), &a)?)),
            "--bench-idle" => o.bench = Some(Bench::Idle(secs(it.next(), &a)?)),
            "--print-diagnostics" => o.print_diagnostics = true,
            "-h" | "--help" => o.help = true,
            other => return Err(format!("unknown argument '{other}' (see --help)")),
        }
    }
    if o.dev {
        o.allow_exit = true;
    }
    Ok(o)
}

impl Options {
    /// Config file values that the command line can only switch on.
    pub fn merge_config(&mut self, c: &crate::config::Config) {
        self.hud |= c.debug.hud;
        self.allow_exit |= c.debug.allow_exit;
        self.double_buffer |= c.display.double_buffer;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Options, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn defaults() {
        assert_eq!(p(&[]).unwrap(), Options::default());
    }

    #[test]
    fn dev_implies_allow_exit() {
        let o = p(&["--dev", "--hud"]).unwrap();
        assert!(o.dev && o.hud && o.allow_exit);
    }

    #[test]
    fn bench_values() {
        assert_eq!(
            p(&["--bench-pacing", "2.5"]).unwrap().bench,
            Some(Bench::Pacing(Duration::from_millis(2500)))
        );
        assert!(p(&["--bench-idle"]).is_err());
        assert!(p(&["--bench-idle", "-1"]).is_err());
        assert!(p(&["--bench-idle", "x"]).is_err());
    }

    #[test]
    fn config_path_and_merge() {
        let mut o = p(&["--config", "/tmp/x.toml"]).unwrap();
        assert_eq!(o.config.as_deref(), Some(std::path::Path::new("/tmp/x.toml")));
        assert!(p(&["--config"]).is_err());
        let mut c = crate::config::Config::default();
        c.debug.hud = true;
        o.merge_config(&c);
        assert!(o.hud && !o.allow_exit && !o.double_buffer);
    }

    #[test]
    fn unknown_flag() {
        assert!(p(&["--nope"]).is_err());
    }
}
