//! The server's WebSocket (`/socket`, spec 8): library and user-data
//! changes made elsewhere, and remote control from other Jellyfin apps.
//!
//! Checked against Jellyfin's server source (`WebSocketManager`,
//! `SessionWebSocketListener`, `WebSocketConnection`):
//! - The upgrade request is authenticated like any API call, so it carries
//!   the `Authorization` header (no token in the URL, where it would end up
//!   in proxy logs).
//! - The socket joins this device's session; the session accepts remote
//!   control only while a socket is open and after the device posted
//!   capabilities with `SupportsMediaControl` (`endpoints::post_capabilities`).
//! - Every message is a JSON text frame `{"MessageType": …, "Data": …}`.
//!   The server sends `ForceKeepAlive` (Data: its timeout in seconds, 60)
//!   and drops a socket that sent nothing for that long; the client sends
//!   `{"MessageType":"KeepAlive"}` well within it and the server answers
//!   `KeepAlive`.
//!
//! The handshake goes through reqwest (HTTP/1.1 upgrade), so the socket
//! uses the same TLS settings as every other request (system roots plus
//! `server.ca_file`); tungstenite only does the framing.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::client::{Client, Error, Result};
use crate::dto::{GeneralCommandType, LibraryUpdateInfo, PlayRequest, PlaystateCommand};
use crate::dto::{UserDataChangeInfo, UserItemDataDto};

/// How long the upgrade may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Jellyfin's default when it doesn't say (`WebSocketLostTimeout`).
pub const DEFAULT_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(60);

/// A message from the server, reduced to what a client acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    /// Send keep-alives: the server drops the socket after this long
    /// without one.
    ForceKeepAlive(Duration),
    /// The answer to a keep-alive.
    KeepAlive,
    LibraryChanged(LibraryUpdateInfo),
    UserDataChanged(UserDataChangeInfo),
    /// Pause, seek, stop… from another app.
    Playstate {
        command: PlaystateCommand,
        seek_ticks: Option<i64>,
    },
    /// Navigation, a message to show, track changes… from another app.
    GeneralCommand {
        name: GeneralCommandType,
        arguments: HashMap<String, String>,
    },
    /// Play these items.
    Play(PlayRequest),
    /// The server is going away (restart or shutdown); reconnect later.
    ServerGoingAway,
    /// This user's account changed or was deleted.
    UserChanged,
    /// Anything else (scheduled tasks, plugins, SyncPlay…): ignored.
    Other(String),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "MessageType", default)]
    kind: String,
    #[serde(rename = "Data", default)]
    data: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlaystateData {
    #[serde(rename = "Command")]
    command: Option<PlaystateCommand>,
    #[serde(rename = "SeekPositionTicks")]
    seek: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommandData {
    #[serde(rename = "Name")]
    name: Option<GeneralCommandType>,
    // Values are strings in the schema; be lenient about numbers.
    #[serde(rename = "Arguments")]
    arguments: Option<HashMap<String, serde_json::Value>>,
}

/// Parses one text frame. Unknown or malformed messages become `Other`
/// (spec 12: a newer server can't break the client).
pub fn parse(text: &str) -> ServerMessage {
    let Ok(env) = serde_json::from_str::<Envelope>(text) else {
        return ServerMessage::Other("unparseable".into());
    };
    let data = env.data;
    let other = |kind: &str| ServerMessage::Other(kind.to_string());
    match env.kind.as_str() {
        "ForceKeepAlive" => ServerMessage::ForceKeepAlive(
            data.as_u64().filter(|s| *s > 0).map_or(DEFAULT_KEEPALIVE_TIMEOUT, Duration::from_secs),
        ),
        "KeepAlive" => ServerMessage::KeepAlive,
        "LibraryChanged" => serde_json::from_value(data)
            .map_or_else(|_| other("LibraryChanged"), ServerMessage::LibraryChanged),
        "UserDataChanged" => serde_json::from_value(data)
            .map_or_else(|_| other("UserDataChanged"), ServerMessage::UserDataChanged),
        "Playstate" => match serde_json::from_value::<PlaystateData>(data) {
            Ok(PlaystateData { command: Some(command), seek }) => {
                ServerMessage::Playstate { command, seek_ticks: seek }
            }
            _ => other("Playstate"),
        },
        "GeneralCommand" => match serde_json::from_value::<CommandData>(data) {
            Ok(CommandData { name: Some(name), arguments }) => ServerMessage::GeneralCommand {
                name,
                arguments: arguments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(k, v)| {
                        let v = match v {
                            serde_json::Value::String(s) => s,
                            v => v.to_string(),
                        };
                        (k, v)
                    })
                    .collect(),
            },
            _ => other("GeneralCommand"),
        },
        "Play" => serde_json::from_value(data).map_or_else(|_| other("Play"), ServerMessage::Play),
        "ServerRestarting" | "ServerShuttingDown" => ServerMessage::ServerGoingAway,
        "UserUpdated" | "UserDeleted" => ServerMessage::UserChanged,
        k => other(k),
    }
}

/// The items a user-data message is about.
pub fn changed_items(info: &UserDataChangeInfo) -> impl Iterator<Item = &UserItemDataDto> {
    info.user_data_list.iter().flatten()
}

/// An open socket.
pub struct Socket {
    ws: WebSocketStream<reqwest::Upgraded>,
}

impl Socket {
    /// The next server message; None when the socket closed. Pings are
    /// answered inside; binary frames are skipped.
    pub async fn next(&mut self) -> Option<Result<ServerMessage>> {
        loop {
            match self.ws.next().await? {
                Ok(Message::Text(t)) => return Some(Ok(parse(t.as_str()))),
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => return Some(Err(Error::Network(format!("websocket: {e}")))),
            }
        }
    }

    pub async fn send_keep_alive(&mut self) -> Result<()> {
        self.ws
            .send(Message::text(r#"{"MessageType":"KeepAlive"}"#))
            .await
            .map_err(|e| Error::Network(format!("websocket: {e}")))
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

impl Client {
    /// Opens the server's WebSocket for this session (needs a token).
    pub async fn open_socket(&self) -> Result<Socket> {
        let key = generate_key();
        let rb = self
            .socket_request("/socket")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", &key);
        let upgrade = async {
            let resp = rb.send().await.map_err(|e| Error::Network(super::client::chain(&e)))?;
            if resp.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
                return Err(Error::Status(resp.status().as_u16()));
            }
            let accept = resp
                .headers()
                .get("Sec-WebSocket-Accept")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if accept != derive_accept_key(key.as_bytes()) {
                return Err(Error::Parse("websocket: wrong Sec-WebSocket-Accept".into()));
            }
            resp.upgrade().await.map_err(|e| Error::Network(super::client::chain(&e)))
        };
        let io = tokio::time::timeout(HANDSHAKE_TIMEOUT, upgrade)
            .await
            .map_err(|_| Error::Network("websocket: handshake timed out".into()))??;
        let ws = WebSocketStream::from_raw_socket(io, Role::Client, None).await;
        Ok(Socket { ws })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_jellyfin_sends() {
        assert_eq!(
            parse(r#"{"MessageType":"ForceKeepAlive","Data":60,"MessageId":"x"}"#),
            ServerMessage::ForceKeepAlive(Duration::from_secs(60))
        );
        assert_eq!(parse(r#"{"MessageType":"KeepAlive"}"#), ServerMessage::KeepAlive);
        let m = parse(
            r#"{"MessageType":"Playstate","Data":{"Command":"Seek","SeekPositionTicks":6000000000}}"#,
        );
        assert_eq!(
            m,
            ServerMessage::Playstate {
                command: PlaystateCommand::Seek,
                seek_ticks: Some(6_000_000_000)
            }
        );
        let m = parse(
            r#"{"MessageType":"GeneralCommand","Data":{"Name":"DisplayMessage",
                "Arguments":{"Header":"Hi","Text":"Dinner","TimeoutMs":5000}}}"#,
        );
        let ServerMessage::GeneralCommand { name, arguments } = m else { panic!("{m:?}") };
        assert_eq!(name, GeneralCommandType::DisplayMessage);
        assert_eq!(arguments["Text"], "Dinner");
        assert_eq!(arguments["TimeoutMs"], "5000");
        let m = parse(
            r#"{"MessageType":"UserDataChanged","Data":{"UserId":"u",
                "UserDataList":[{"ItemId":"i1","Played":true,"IsFavorite":false}]}}"#,
        );
        let ServerMessage::UserDataChanged(info) = m else { panic!() };
        assert_eq!(changed_items(&info).next().unwrap().item_id.as_deref(), Some("i1"));
        let m = parse(
            r#"{"MessageType":"Play","Data":{"ItemIds":["a"],"PlayCommand":"PlayNow",
                "StartPositionTicks":0}}"#,
        );
        let ServerMessage::Play(p) = m else { panic!() };
        assert_eq!(p.item_ids.unwrap(), ["a"]);
    }

    #[test]
    fn unknown_and_broken_messages_are_ignored() {
        assert!(
            matches!(parse(r#"{"MessageType":"SyncPlayCommand","Data":{}}"#), ServerMessage::Other(k) if k == "SyncPlayCommand")
        );
        assert!(matches!(parse("not json"), ServerMessage::Other(_)));
        assert!(matches!(
            parse(r#"{"MessageType":"Playstate","Data":{"Command":"Warp"}}"#),
            ServerMessage::Playstate { command: PlaystateCommand::Unrecognized, .. }
        ));
        assert!(matches!(
            parse(r#"{"MessageType":"Playstate","Data":5}"#),
            ServerMessage::Other(_)
        ));
    }
}
