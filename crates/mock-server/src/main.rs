//! `jellyroll-mock`: run the mock Jellyfin server (spec 13).
//!
//! Point jellyroll at it with `[server] url = "http://127.0.0.1:8096"`, or
//! let LAN discovery find it. Quick Connect approves itself after a few
//! seconds; the code shown is always 424242.

#![forbid(unsafe_code)]

use std::time::Duration;

use jellyroll_mock_server::{MockConfig, discovery, serve};

const USAGE: &str = "\
jellyroll-mock: mock Jellyfin server for development and CI

USAGE: jellyroll-mock [OPTIONS]

  --port N            HTTP port (default 8096)
  --movies N          Movies in the library (default 40; 10000 for scroll tests)
  --approve-after N   Quick Connect polls before approval (default 2)
  --latency-ms N      Delay every response (default 0)
  --version V         Jellyfin version to report (default 10.11.0)
  --no-quick-connect  Report Quick Connect as disabled
  --no-discovery      Don't answer LAN discovery on UDP 7359
";

fn main() {
    let mut cfg = MockConfig::default();
    let mut port: u16 = 8096;
    let mut discover = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n\n{USAGE}");
                std::process::exit(2)
            })
        };
        let num = |s: String| -> u64 {
            s.parse().unwrap_or_else(|_| {
                eprintln!("not a number: {s}");
                std::process::exit(2)
            })
        };
        match a.as_str() {
            "--port" => port = num(val("--port")) as u16,
            "--movies" => cfg.movies = num(val("--movies")) as usize,
            "--approve-after" => cfg.approve_after = num(val("--approve-after")) as u32,
            "--latency-ms" => cfg.latency = Duration::from_millis(num(val("--latency-ms"))),
            "--version" => cfg.version = val("--version"),
            "--no-quick-connect" => cfg.quick_connect_enabled = false,
            "--no-discovery" => discover = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("jellyroll-mock: port {port}: {e}");
                std::process::exit(1);
            }
        };
        println!(
            "jellyroll-mock: http://0.0.0.0:{port} ({} movies, Quick Connect code 424242)",
            cfg.movies
        );
        if discover {
            match tokio::net::UdpSocket::bind(("0.0.0.0", 7359)).await {
                Ok(sock) => {
                    let addr = format!("http://127.0.0.1:{port}");
                    tokio::spawn(discovery(sock, cfg.server_name.clone(), addr));
                }
                Err(e) => eprintln!("jellyroll-mock: discovery disabled (UDP 7359: {e})"),
            }
        }
        serve(listener, cfg).await;
    });
}
