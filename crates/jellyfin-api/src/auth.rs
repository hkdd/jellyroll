//! Client identity and the `Authorization` header (spec 8: device identity).
//!
//! Format matches the official Jellyfin TypeScript SDK
//! (`getAuthorizationHeader`): `MediaBrowser Client="…", Device="…",
//! DeviceId="…", Version="…", Token="…"`, each value encoded like
//! JavaScript's `encodeURIComponent`.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    /// App name shown in the server's device list.
    pub client: String,
    pub version: String,
    /// Device name shown in the server's device list (e.g. the hostname).
    pub device: String,
    /// Stable per installation; the server keys sessions and Quick Connect
    /// on it.
    pub device_id: String,
}

impl ClientInfo {
    pub fn authorization(&self, token: Option<&str>) -> String {
        let mut s = String::with_capacity(160);
        s.push_str("MediaBrowser Client=\"");
        encode_component(&mut s, &self.client);
        s.push_str("\", Device=\"");
        encode_component(&mut s, &self.device);
        s.push_str("\", DeviceId=\"");
        encode_component(&mut s, &self.device_id);
        s.push_str("\", Version=\"");
        encode_component(&mut s, &self.version);
        s.push_str("\", Token=\"");
        encode_component(&mut s, token.unwrap_or(""));
        s.push('"');
        s
    }
}

/// JavaScript `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )`
/// is percent-encoded as UTF-8.
pub fn encode_component(out: &mut String, s: &str) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ClientInfo {
        ClientInfo {
            client: "Jellyroll".into(),
            version: "0.1.0".into(),
            device: "living room".into(),
            device_id: "abc123".into(),
        }
    }

    #[test]
    fn header_matches_sdk_format() {
        assert_eq!(
            info().authorization(None),
            r#"MediaBrowser Client="Jellyroll", Device="living%20room", DeviceId="abc123", Version="0.1.0", Token="""#
        );
        assert!(info().authorization(Some("t0k")).ends_with(r#"Token="t0k""#));
    }

    #[test]
    fn encodes_like_encode_uri_component() {
        let mut s = String::new();
        encode_component(&mut s, "a b\"c,d=é(x)!~*'");
        assert_eq!(s, "a%20b%22c%2Cd%3D%C3%A9(x)!~*'");
    }
}
