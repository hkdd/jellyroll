//! Hand-rolled CLI parsing (no extra dependency for a dozen flags).

use std::time::Duration;

pub const USAGE: &str = "\
jellyroll: boot-to-app Jellyfin HTPC client

USAGE: jellyroll [OPTIONS]

  --config PATH          Config file (default: ~/.config/jellyroll/config.toml)
  --dev                  Run in a desktop window (Wayland/X11/cage) instead of KMSDRM
  --window WxH           Dev window size in pixels, e.g. 3840x2160 (with --dev)
  --hud                  Show the perf HUD at startup (toggle: F3, or Info x3 later)
  --allow-exit           Let Esc/Back on the top level quit (always on with --dev)
  --double-buffer        Request double instead of triple buffering (KMSDRM)
  --no-vsync             Disable vsync (benchmarking only)
  --play URL             Play a file or URL with mpv (M3 harness). Keys: Space
                         pause, Left/Right seek 10 s, Up/Down 60 s, i diagnostics,
                         Esc/Back stop. Exits when playback ends
  --bench-play SECS      With --play: measure playback for SECS (dropped frames,
                         missed vsyncs, CPU), toggling the OSD every 2 s
  --demo                 Show the synthetic render benchmark scene instead of
                         connecting to a server (no network)
  --bench-pacing SECS    Animate the demo scene for SECS, print frame stats, exit
                         non-zero if any budget is exceeded (implies --demo)
  --bench-idle SECS      Sit on the static demo scene for SECS, verify no redraws
                         and < 1% CPU, exit non-zero otherwise (implies --demo)
  --bench-scroll SECS    Scroll a 10,000-item library grid (built-in mock server,
                         no account needed) for SECS, print frame stats, exit
                         non-zero if a budget is exceeded
  --bench-upload         Time ways of uploading artwork to the GPU and exit
  --soak HOURS           Drive the UI for HOURS against the built-in mock server
                         (no account needed), log memory every minute to
                         soak.csv, exit non-zero if it keeps growing
  --soak-media FILE      With --soak: also play FILE for a minute each cycle
  --print-diagnostics    Print SDL/GL/display information and exit
  -h, --help             Show this help
";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub config: Option<std::path::PathBuf>,
    pub dev: bool,
    pub window: Option<(u32, u32)>,
    pub hud: bool,
    pub allow_exit: bool,
    pub double_buffer: bool,
    pub no_vsync: bool,
    /// Synthetic M2 scene instead of the Jellyfin home screen.
    pub demo: bool,
    pub bench: Option<Bench>,
    pub play: Option<String>,
    pub print_diagnostics: bool,
    pub bench_upload: bool,
    /// `--soak`: how long to run.
    pub soak: Option<Duration>,
    pub soak_media: Option<std::path::PathBuf>,
    pub help: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Bench {
    Pacing(Duration),
    Idle(Duration),
    Play(Duration),
    Scroll(Duration),
}

fn secs(v: Option<String>, flag: &str) -> Result<Duration, String> {
    let v = v.ok_or_else(|| format!("{flag} needs a value in seconds"))?;
    let s: f64 = v.parse().map_err(|_| format!("{flag}: invalid number '{v}'"))?;
    if !(s > 0.0 && s.is_finite()) {
        return Err(format!("{flag}: must be > 0"));
    }
    Ok(Duration::from_secs_f64(s))
}

fn size(v: Option<String>, flag: &str) -> Result<(u32, u32), String> {
    let v = v.ok_or_else(|| format!("{flag} needs a size like 1920x1080"))?;
    let (w, h) =
        v.split_once(['x', 'X']).ok_or_else(|| format!("{flag}: expected WxH, got '{v}'"))?;
    match (w.parse(), h.parse()) {
        (Ok(w @ 1..=16384), Ok(h @ 1..=16384)) => Ok((w, h)),
        _ => Err(format!("{flag}: invalid size '{v}'")),
    }
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
            "--window" => o.window = Some(size(it.next(), &a)?),
            "--hud" => o.hud = true,
            "--allow-exit" => o.allow_exit = true,
            "--double-buffer" => o.double_buffer = true,
            "--no-vsync" => o.no_vsync = true,
            "--demo" => o.demo = true,
            "--bench-pacing" => o.bench = Some(Bench::Pacing(secs(it.next(), &a)?)),
            "--bench-idle" => o.bench = Some(Bench::Idle(secs(it.next(), &a)?)),
            "--bench-play" => o.bench = Some(Bench::Play(secs(it.next(), &a)?)),
            "--bench-scroll" => o.bench = Some(Bench::Scroll(secs(it.next(), &a)?)),
            "--play" => o.play = Some(it.next().ok_or("--play needs a file or URL")?),
            "--print-diagnostics" => o.print_diagnostics = true,
            "--bench-upload" => o.bench_upload = true,
            "--soak" => {
                let v = it.next().ok_or("--soak needs a value in hours")?;
                let h: f64 = v.parse().map_err(|_| format!("--soak: invalid number '{v}'"))?;
                if !(h > 0.0 && h.is_finite()) {
                    return Err("--soak: hours must be positive".into());
                }
                o.soak = Some(Duration::from_secs_f64(h * 3600.0));
            }
            "--soak-media" => {
                o.soak_media = Some(it.next().ok_or("--soak-media needs a file")?.into())
            }
            "-h" | "--help" => o.help = true,
            other => return Err(format!("unknown argument '{other}' (see --help)")),
        }
    }
    if o.dev {
        o.allow_exit = true;
    }
    // Render benchmarks measure the renderer, not the network: they always
    // run on the deterministic demo scene.
    if matches!(o.bench, Some(Bench::Pacing(_) | Bench::Idle(_))) {
        o.demo = true;
    }
    match (o.bench, &o.play) {
        (Some(Bench::Play(_)), None) => return Err("--bench-play needs --play URL".into()),
        (Some(Bench::Pacing(_) | Bench::Idle(_) | Bench::Scroll(_)), Some(_)) => {
            return Err(
                "--bench-pacing/--bench-idle/--bench-scroll cannot be combined with --play".into(),
            );
        }
        (Some(Bench::Scroll(_)), None) if o.demo => {
            return Err("--bench-scroll scrolls the library grid, not the --demo scene".into());
        }
        _ => {}
    }
    if o.soak.is_some() && (o.bench.is_some() || o.play.is_some() || o.demo) {
        return Err("--soak runs on its own (no --bench-*, --play or --demo)".into());
    }
    if o.soak_media.is_some() && o.soak.is_none() {
        return Err("--soak-media needs --soak HOURS".into());
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
        let o = p(&["--bench-scroll", "10"]).unwrap();
        assert_eq!(o.bench, Some(Bench::Scroll(Duration::from_secs(10))));
        assert!(!o.demo, "scrolls the library, not the demo scene");
        assert!(p(&["--bench-scroll", "10", "--demo"]).is_err());
        assert!(p(&["--bench-scroll", "10", "--play", "x"]).is_err());
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
    fn window_size() {
        assert_eq!(p(&["--window", "3840x2160"]).unwrap().window, Some((3840, 2160)));
        assert!(p(&["--window", "big"]).is_err());
        assert!(p(&["--window", "0x10"]).is_err());
        assert!(p(&["--window"]).is_err());
    }

    #[test]
    fn play_flags() {
        let o = p(&["--play", "/m/a.mkv", "--bench-play", "5"]).unwrap();
        assert_eq!(o.play.as_deref(), Some("/m/a.mkv"));
        assert_eq!(o.bench, Some(Bench::Play(Duration::from_secs(5))));
        assert!(p(&["--bench-play", "5"]).is_err());
        assert!(p(&["--play", "x", "--bench-idle", "1"]).is_err());
        assert!(p(&["--play"]).is_err());
    }

    #[test]
    fn unknown_flag() {
        assert!(p(&["--nope"]).is_err());
    }
}
