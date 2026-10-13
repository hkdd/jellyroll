//! Reboot and power off through systemd-logind over D-Bus (spec 5.5).
//!
//! logind asks polkit whether the user may; on an HTPC with no one to type
//! a password the answer must be "yes", which the polkit rule in
//! docs/polkit-jellyroll.rules grants the jellyroll user. Calls run on a
//! short-lived thread with its own runtime, so the render loop never
//! waits on the bus.

use std::sync::{Arc, OnceLock};

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const IFACE: &str = "org.freedesktop.login1.Manager";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Reboot,
    PowerOff,
}

impl Action {
    fn method(self) -> &'static str {
        match self {
            Action::Reboot => "Reboot",
            Action::PowerOff => "PowerOff",
        }
    }

    fn can_method(self) -> &'static str {
        match self {
            Action::Reboot => "CanReboot",
            Action::PowerOff => "CanPowerOff",
        }
    }
}

/// logind's answer to CanReboot/CanPowerOff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Allowed {
    Yes,
    /// Only with authentication (no polkit rule for this user).
    NeedsAuth,
    No,
    /// logind not reachable (no system bus, not systemd).
    Unknown(String),
}

impl Allowed {
    fn from_logind(s: &str) -> Self {
        match s {
            "yes" => Allowed::Yes,
            "challenge" => Allowed::NeedsAuth,
            other => {
                if other == "no" || other == "na" {
                    Allowed::No
                } else {
                    Allowed::Unknown(format!("logind says \"{other}\""))
                }
            }
        }
    }
}

/// Where a request's answer appears (Ok: logind accepted it).
pub type Answer = Arc<OnceLock<Result<(), String>>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Caps {
    pub reboot: Allowed,
    pub power_off: Allowed,
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))
}

async fn call_can(conn: &zbus::Connection, a: Action) -> Allowed {
    let reply = conn.call_method(Some(DEST), PATH, Some(IFACE), a.can_method(), &()).await;
    match reply.and_then(|m| m.body().deserialize::<String>()) {
        Ok(s) => Allowed::from_logind(&s),
        Err(e) => Allowed::Unknown(e.to_string()),
    }
}

/// Asks logind what this user may do (blocking; call off the render thread).
pub fn query() -> Caps {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            return Caps { reboot: Allowed::Unknown(e.clone()), power_off: Allowed::Unknown(e) };
        }
    };
    rt.block_on(async {
        match zbus::Connection::system().await {
            Ok(conn) => Caps {
                reboot: call_can(&conn, Action::Reboot).await,
                power_off: call_can(&conn, Action::PowerOff).await,
            },
            Err(e) => {
                let e = format!("system bus: {e}");
                Caps { reboot: Allowed::Unknown(e.clone()), power_off: Allowed::Unknown(e) }
            }
        }
    })
}

/// Queries in the background; the result appears in the cell.
pub fn query_in_background() -> Arc<OnceLock<Caps>> {
    let cell = Arc::new(OnceLock::new());
    let c = cell.clone();
    let _ = std::thread::Builder::new().name("logind-query".into()).spawn(move || {
        let caps = query();
        tracing::info!(?caps, "power actions");
        let _ = c.set(caps);
    });
    cell
}

/// Asks logind to reboot or power off, non-interactively (no password
/// prompt is possible). On success the system goes down shortly; the
/// result cell holds the error otherwise.
pub fn request(a: Action) -> Answer {
    let cell = Arc::new(OnceLock::new());
    let c = cell.clone();
    let _ = std::thread::Builder::new().name("logind-call".into()).spawn(move || {
        let r = runtime().and_then(|rt| {
            rt.block_on(async {
                let conn =
                    zbus::Connection::system().await.map_err(|e| format!("system bus: {e}"))?;
                conn.call_method(Some(DEST), PATH, Some(IFACE), a.method(), &(false,))
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        });
        match &r {
            Ok(()) => tracing::info!(?a, "logind accepted the power action"),
            Err(e) => tracing::error!(?a, "power action failed: {e}"),
        }
        let _ = c.set(r);
    });
    cell
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logind_answers() {
        assert_eq!(Allowed::from_logind("yes"), Allowed::Yes);
        assert_eq!(Allowed::from_logind("challenge"), Allowed::NeedsAuth);
        assert_eq!(Allowed::from_logind("no"), Allowed::No);
        assert_eq!(Allowed::from_logind("na"), Allowed::No);
        assert!(matches!(Allowed::from_logind("odd"), Allowed::Unknown(_)));
    }

    #[test]
    fn no_bus_is_unknown_not_a_panic() {
        // The test environment may or may not have a system bus; either
        // way the query returns an answer.
        let c = query();
        let _ = (c.reboot, c.power_off);
    }
}
