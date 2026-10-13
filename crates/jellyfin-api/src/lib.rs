//! Jellyfin client (spec 8). `dto` is generated from the server's OpenAPI
//! spec by `scripts/gen-jellyfin-dto.py`; auth, endpoints, the HTTP client
//! and discovery are hand-written.

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod discovery;
#[rustfmt::skip]
pub mod dto;
pub mod endpoints;

pub use auth::ClientInfo;
pub use client::{Client, Error, TlsOptions, normalize_server_url};

/// Oldest server this client is written for: the same floor the official
/// Jellyfin SDK uses (docs/VERSIONS.md).
pub const MIN_SERVER_VERSION: (u32, u32, u32) = (10, 10, 0);

/// Parses "10.11.2" (or "10.11.2.0") into a comparable triple.
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn server_supported(version: &str) -> bool {
    parse_version(version).is_some_and(|v| v >= MIN_SERVER_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_floor() {
        assert!(server_supported("10.10.0"));
        assert!(server_supported("10.11.3"));
        assert!(server_supported("12.2.0"));
        assert!(!server_supported("10.9.11"));
        assert!(!server_supported("garbage"));
    }

    #[test]
    fn tolerant_dtos() {
        // Unknown fields, unknown enum values, nulls and missing fields are
        // all fine (spec 12).
        let j = br#"{"Items":[{"Id":"1","Name":"X","Type":"HoloDeck","Extra":5,
                     "UserData":null,"ImageTags":{"Primary":"t"}}],"TotalRecordCount":null}"#;
        let r: dto::BaseItemDtoQueryResult = serde_json::from_slice(j).unwrap();
        let it = &r.items.as_ref().unwrap()[0];
        assert_eq!(it.r#type, Some(dto::BaseItemKind::Unrecognized));
        assert_eq!(it.image_tags.as_ref().unwrap()["Primary"].as_deref(), Some("t"));
        assert!(it.user_data.is_none());
    }
}
