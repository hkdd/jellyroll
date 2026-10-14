//! Async HTTP client over reqwest (rustls). Runs on the app's network
//! thread; nothing here touches the render thread (spec 4).

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::auth::ClientInfo;
use crate::endpoints::{Method, Request};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Server unreachable, timeout, TLS failure...
    Network(String),
    /// HTTP error status; 401 means the token is no longer valid.
    Status(u16),
    /// The body wasn't what the API describes.
    Parse(String),
    Config(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Network(e) => write!(f, "network error: {e}"),
            Error::Status(401) => write!(f, "not signed in (HTTP 401)"),
            Error::Status(s) => write!(f, "server returned HTTP {s}"),
            Error::Parse(e) => write!(f, "unexpected server response: {e}"),
            Error::Config(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// TLS settings beyond the system roots (spec 12): an extra CA or a
/// self-signed server certificate, as PEM.
#[derive(Clone, Debug, Default)]
pub struct TlsOptions {
    pub extra_root_pem: Option<Vec<u8>>,
}

/// Normalizes what a user types or discovery reports into a base URL:
/// adds `http://` when no scheme is given, Jellyfin's default port 8096
/// when no port is given on plain http, and drops trailing slashes.
pub fn normalize_server_url(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let (scheme, rest) = match s.split_once("://") {
        Some((sc, rest)) => (sc.to_ascii_lowercase(), rest),
        None => ("http".to_string(), s),
    };
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if host.is_empty() {
        return None;
    }
    // A port is present if the part after the last ':' is numeric and the
    // host isn't a bare IPv6 address ([::1] keeps its brackets).
    let has_port = host.rsplit_once(':').is_some_and(|(h, p)| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && (!h.contains(':') || h.ends_with(']'))
    });
    let port = if has_port || scheme == "https" || !path.is_empty() { "" } else { ":8096" };
    Some(format!("{scheme}://{host}{port}{path}"))
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// The same settings without the request timeout, for the WebSocket
    /// (a long-lived connection).
    socket_http: reqwest::Client,
    base: String,
    info: ClientInfo,
    token: Option<String>,
}

impl Client {
    pub fn new(base_url: &str, info: ClientInfo, tls: &TlsOptions) -> Result<Self> {
        let base = normalize_server_url(base_url)
            .ok_or_else(|| Error::Config(format!("invalid server address {base_url:?}")))?;
        let build = |timeout: Option<Duration>| {
            let mut b = reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .user_agent(format!("{}/{}", info.client, info.version));
            if let Some(t) = timeout {
                b = b.timeout(t);
            }
            if let Some(pem) = &tls.extra_root_pem {
                let cert = reqwest::Certificate::from_pem(pem)
                    .map_err(|e| Error::Config(format!("custom CA certificate: {e}")))?;
                b = b.tls_certs_merge([cert]);
            }
            b.build().map_err(|e| Error::Config(format!("HTTP client: {e}")))
        };
        let http = build(Some(REQUEST_TIMEOUT))?;
        let socket_http = build(None)?;
        Ok(Self { http, socket_http, base, info, token: None })
    }

    /// The `Authorization` header this client sends, for other HTTP users
    /// of the same session (mpv streaming a video).
    pub fn authorization_header(&self) -> String {
        self.info.authorization(self.token.as_deref())
    }

    /// The short `MediaBrowser Token="…"` form, for HTTP clients whose
    /// header option can't carry commas (mpv). The server accepts it.
    pub fn token_header(&self) -> String {
        let mut s = String::from("MediaBrowser Token=\"");
        crate::auth::encode_component(&mut s, self.token.as_deref().unwrap_or(""));
        s.push('"');
        s
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base, path);
        let rb = match method {
            Method::Get => self.http.get(url),
            Method::Post => self.http.post(url),
            Method::Delete => self.http.delete(url),
        };
        rb.header("Authorization", self.info.authorization(self.token.as_deref()))
    }

    /// A GET for the WebSocket upgrade (no request timeout).
    pub(crate) fn socket_request(&self, path: &str) -> reqwest::RequestBuilder {
        self.socket_http
            .get(format!("{}{}", self.base, path))
            .header("Authorization", self.info.authorization(self.token.as_deref()))
    }

    pub async fn send<T: DeserializeOwned>(&self, req: &Request<T>) -> Result<T> {
        let mut rb = self.request(req.method, &req.path).header("Accept", "application/json");
        if let Some(body) = &req.body {
            rb = rb.header("Content-Type", "application/json").body(body.clone());
        } else if req.method != Method::Get {
            rb = rb.header("Content-Length", "0");
        }
        let bytes = Self::execute(rb).await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Parse(format!("{}: {e}", req.path)))
    }

    /// For endpoints that answer 204 No Content.
    pub async fn send_no_content<T>(&self, req: &Request<T>) -> Result<()> {
        let mut rb = self.request(req.method, &req.path);
        if let Some(body) = &req.body {
            rb = rb.header("Content-Type", "application/json").body(body.clone());
        } else if req.method != Method::Get {
            rb = rb.header("Content-Length", "0");
        }
        Self::execute(rb).await.map(|_| ())
    }

    /// Raw bytes, e.g. an image.
    pub async fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        Self::execute(self.request(Method::Get, path)).await
    }

    async fn execute(rb: reqwest::RequestBuilder) -> Result<Vec<u8>> {
        let resp = rb.send().await.map_err(|e| Error::Network(chain(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Status(status.as_u16()));
        }
        let bytes = resp.bytes().await.map_err(|e| Error::Network(chain(&e)))?;
        Ok(bytes.to_vec())
    }
}

/// reqwest's top-level message is generic ("error sending request"); the
/// cause chain says what actually happened (refused, timed out, TLS...).
pub(crate) fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(c) = src {
        s.push_str(": ");
        s.push_str(&c.to_string());
        src = c.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_normalization() {
        let n = |s| normalize_server_url(s);
        assert_eq!(n("192.168.1.5").as_deref(), Some("http://192.168.1.5:8096"));
        assert_eq!(n("jelly.lan:8920/").as_deref(), Some("http://jelly.lan:8920"));
        assert_eq!(n("https://jf.example.com").as_deref(), Some("https://jf.example.com"));
        assert_eq!(n("HTTP://host").as_deref(), Some("http://host:8096"));
        assert_eq!(n("http://host/jellyfin/").as_deref(), Some("http://host/jellyfin"));
        assert_eq!(n("[fd00::5]").as_deref(), Some("http://[fd00::5]:8096"));
        assert_eq!(n("[fd00::5]:9000").as_deref(), Some("http://[fd00::5]:9000"));
        assert_eq!(n(""), None);
        assert_eq!(n("ftp://x"), None);
    }
}
