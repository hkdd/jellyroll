//! Requests as plain data: method, path with query, optional JSON body, and
//! the response type. Building them is pure, so they are unit-tested without
//! a server; `Client::send` performs them.
//!
//! Queries ask for minimal payloads (spec 8): only the extra fields the UI
//! shows, one image of each type, no total counts. Array parameters are
//! comma-separated, which is how Jellyfin binds them.

use std::fmt::Write;
use std::marker::PhantomData;

use crate::auth::encode_component;
use crate::dto::{
    AuthenticationResult, BaseItemDto, BaseItemDtoQueryResult, PublicSystemInfo,
    QuickConnectResult, UserDto, UserItemDataDto,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

#[derive(Debug)]
pub struct Request<T> {
    pub method: Method,
    /// Path and query, already encoded, e.g. `/Shows/NextUp?userId=…`.
    pub path: String,
    pub body: Option<String>,
    _response: PhantomData<fn() -> T>,
}

impl<T> Request<T> {
    fn new(method: Method, path: String) -> Self {
        Self { method, path, body: None, _response: PhantomData }
    }

    fn json(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body.to_string());
        self
    }
}

/// Fields beyond the defaults that rows need.
const ROW_FIELDS: &str = "PrimaryImageAspectRatio,Overview";
/// Image types rows draw: poster, backdrop (hero), thumb (landscape cards).
const ROW_IMAGES: &str = "Primary,Backdrop,Thumb";

struct Query(String);

impl Query {
    fn new(path: &str) -> Self {
        Self(path.to_string())
    }

    fn param(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.0.push(if self.0.contains('?') { '&' } else { '?' });
        self.0.push_str(key);
        self.0.push('=');
        let mut v = String::new();
        let _ = write!(v, "{value}");
        encode_component(&mut self.0, &v);
        self
    }

    /// The common row options: minimal fields, one image per type, user
    /// data (played/progress), no total count.
    fn row(self) -> Self {
        self.images().param("enableTotalRecordCount", false)
    }

    /// Fields, images and user data, without the total-count switch.
    fn images(self) -> Self {
        self.param("fields", ROW_FIELDS)
            .param("enableImageTypes", ROW_IMAGES)
            .param("imageTypeLimit", 1)
            .param("enableUserData", true)
    }

    fn opt(self, key: &str, value: Option<impl std::fmt::Display>) -> Self {
        match value {
            Some(v) => self.param(key, v),
            None => self,
        }
    }
}

pub fn public_info() -> Request<PublicSystemInfo> {
    Request::new(Method::Get, "/System/Info/Public".into())
}

pub fn quick_connect_enabled() -> Request<bool> {
    Request::new(Method::Get, "/QuickConnect/Enabled".into())
}

/// Starts Quick Connect: the result carries the code to show on screen and
/// the secret to poll with.
pub fn quick_connect_initiate() -> Request<QuickConnectResult> {
    Request::new(Method::Post, "/QuickConnect/Initiate".into())
}

pub fn quick_connect_state(secret: &str) -> Request<QuickConnectResult> {
    Request::new(Method::Get, Query::new("/QuickConnect/Connect").param("secret", secret).0)
}

pub fn authenticate_quick_connect(secret: &str) -> Request<AuthenticationResult> {
    Request::new(Method::Post, "/Users/AuthenticateWithQuickConnect".into())
        .json(serde_json::json!({ "Secret": secret }))
}

pub fn authenticate_by_name(username: &str, password: &str) -> Request<AuthenticationResult> {
    Request::new(Method::Post, "/Users/AuthenticateByName".into())
        .json(serde_json::json!({ "Username": username, "Pw": password }))
}

/// The user's libraries ("My Media").
pub fn user_views(user_id: &str) -> Request<BaseItemDtoQueryResult> {
    Request::new(Method::Get, Query::new("/UserViews").param("userId", user_id).0)
}

/// Continue Watching.
pub fn resume(user_id: &str, limit: u32) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new("/UserItems/Resume")
        .param("userId", user_id)
        .param("limit", limit)
        .param("mediaTypes", "Video")
        .row();
    Request::new(Method::Get, q.0)
}

/// Next Up. Resumable and rewatched episodes are left to Continue Watching,
/// as jellyfin-web does on its home screen.
pub fn next_up(user_id: &str, limit: u32) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new("/Shows/NextUp")
        .param("userId", user_id)
        .param("limit", limit)
        .param("enableResumable", false)
        .param("enableRewatching", false)
        .row();
    Request::new(Method::Get, q.0)
}

/// Latest additions to one library.
pub fn latest(user_id: &str, parent_id: &str, limit: u32) -> Request<Vec<BaseItemDto>> {
    let q = Query::new("/Items/Latest")
        .param("userId", user_id)
        .param("parentId", parent_id)
        .param("limit", limit)
        .row();
    Request::new(Method::Get, q.0)
}

/// Favorite movies and shows, by name.
pub fn favorites(user_id: &str, limit: u32) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new("/Items")
        .param("userId", user_id)
        .param("isFavorite", true)
        .param("recursive", true)
        .param("includeItemTypes", "Movie,Series")
        .param("sortBy", "SortName")
        .param("limit", limit)
        .row();
    Request::new(Method::Get, q.0)
}

/// A library listing (grid page, search, alphabet index).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemsQuery {
    pub parent_id: Option<String>,
    /// Comma-separated `BaseItemKind`s, e.g. "Movie" or "Series".
    pub include_types: Option<String>,
    /// Comma-separated `ItemSortBy`s; default "SortName".
    pub sort_by: Option<String>,
    pub descending: bool,
    pub unplayed: bool,
    pub favorites: bool,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub search: Option<String>,
    /// Only items whose sort name sorts before this (alphabet jump: with
    /// `limit` 0, the total count is the index of the first item at or
    /// after it).
    pub name_less_than: Option<String>,
    pub start: u32,
    pub limit: u32,
}

/// One page of items, with the total count.
pub fn items(user_id: &str, q: &ItemsQuery) -> Request<BaseItemDtoQueryResult> {
    let mut filters = Vec::new();
    if q.unplayed {
        filters.push("IsUnplayed");
    }
    if q.favorites {
        filters.push("IsFavorite");
    }
    let path = Query::new("/Items")
        .param("userId", user_id)
        .opt("parentId", q.parent_id.as_deref())
        .param("recursive", true)
        .opt("includeItemTypes", q.include_types.as_deref())
        .param("sortBy", q.sort_by.as_deref().unwrap_or("SortName"))
        .param("sortOrder", if q.descending { "Descending" } else { "Ascending" })
        .opt("filters", (!filters.is_empty()).then(|| filters.join(",")))
        .opt("genres", q.genre.as_deref())
        .opt("years", q.year)
        .opt("searchTerm", q.search.as_deref())
        .opt("nameLessThan", q.name_less_than.as_deref())
        .param("startIndex", q.start)
        .param("limit", q.limit)
        .images()
        .param("enableTotalRecordCount", true)
        .0;
    Request::new(Method::Get, path)
}

/// One item with all its details (cast, media streams, genres...).
pub fn item(user_id: &str, item_id: &str) -> Request<BaseItemDto> {
    let mut path = String::from("/Items/");
    encode_component(&mut path, item_id);
    Request::new(Method::Get, Query::new(&path).param("userId", user_id).0)
}

fn show_path(series_id: &str, what: &str) -> String {
    let mut path = String::from("/Shows/");
    encode_component(&mut path, series_id);
    path.push('/');
    path.push_str(what);
    path
}

pub fn seasons(user_id: &str, series_id: &str) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new(&show_path(series_id, "Seasons")).param("userId", user_id).images();
    Request::new(Method::Get, q.0)
}

pub fn episodes(
    user_id: &str,
    series_id: &str,
    season_id: &str,
) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new(&show_path(series_id, "Episodes"))
        .param("userId", user_id)
        .param("seasonId", season_id)
        .images();
    Request::new(Method::Get, q.0)
}

pub fn similar(user_id: &str, item_id: &str, limit: u32) -> Request<BaseItemDtoQueryResult> {
    let mut path = String::from("/Items/");
    encode_component(&mut path, item_id);
    path.push_str("/Similar");
    let q = Query::new(&path)
        .param("userId", user_id)
        .param("limit", limit)
        .param("fields", ROW_FIELDS);
    Request::new(Method::Get, q.0)
}

/// Genres present in a library, for the grid's filter.
pub fn genres(user_id: &str, parent_id: &str) -> Request<BaseItemDtoQueryResult> {
    let q = Query::new("/Genres")
        .param("userId", user_id)
        .param("parentId", parent_id)
        .param("sortBy", "SortName")
        .param("enableTotalRecordCount", false);
    Request::new(Method::Get, q.0)
}

/// Users shown on the server's sign-in screen (for the user picker).
pub fn public_users() -> Request<Vec<UserDto>> {
    Request::new(Method::Get, "/Users/Public".into())
}

fn user_item(base: &str, user_id: &str, item_id: &str) -> String {
    let mut path = String::from(base);
    encode_component(&mut path, item_id);
    Query::new(&path).param("userId", user_id).0
}

pub fn set_favorite(user_id: &str, item_id: &str, on: bool) -> Request<UserItemDataDto> {
    let method = if on { Method::Post } else { Method::Delete };
    Request::new(method, user_item("/UserFavoriteItems/", user_id, item_id))
}

pub fn set_played(user_id: &str, item_id: &str, on: bool) -> Request<UserItemDataDto> {
    let method = if on { Method::Post } else { Method::Delete };
    Request::new(method, user_item("/UserPlayedItems/", user_id, item_id))
}

/// Ends this device's session and revokes its token on the server.
pub fn logout() -> Request<()> {
    Request::new(Method::Post, "/Sessions/Logout".into())
}

/// Direct stream of a video's original file (direct play: no transcoding,
/// D19). Authenticated by the `Authorization` header, not the URL.
pub fn stream_path(item_id: &str, media_source_id: &str) -> String {
    let mut path = String::from("/Videos/");
    encode_component(&mut path, item_id);
    path.push_str("/stream");
    Query::new(&path).param("static", true).param("mediaSourceId", media_source_id).0
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ImageType {
    Primary,
    Backdrop,
    Thumb,
    Logo,
}

impl ImageType {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageType::Primary => "Primary",
            ImageType::Backdrop => "Backdrop",
            ImageType::Thumb => "Thumb",
            ImageType::Logo => "Logo",
        }
    }
}

/// A pre-sized image (spec 8: never download full-size art). `tag` makes
/// the URL change when the artwork does, so it doubles as the cache key.
/// The server scales to fit within `max_w`×`max_h`, keeping aspect.
pub fn image_path(item_id: &str, kind: ImageType, tag: &str, max_w: u32, max_h: u32) -> String {
    let mut path = String::from("/Items/");
    encode_component(&mut path, item_id);
    path.push_str("/Images/");
    path.push_str(kind.as_str());
    Query::new(&path)
        .param("tag", tag)
        .param("maxWidth", max_w)
        .param("maxHeight", max_h)
        .param("quality", 90)
        .param("format", "Webp")
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_queries_are_minimal() {
        let r = next_up("u1", 20);
        assert_eq!(r.method, Method::Get);
        assert_eq!(
            r.path,
            "/Shows/NextUp?userId=u1&limit=20&enableResumable=false&enableRewatching=false\
             &fields=PrimaryImageAspectRatio%2COverview&enableImageTypes=Primary%2CBackdrop%2CThumb\
             &imageTypeLimit=1&enableUserData=true&enableTotalRecordCount=false"
        );
        assert!(latest("u1", "lib 2", 16).path.contains("parentId=lib%202&limit=16"));
        assert!(resume("u1", 12).path.contains("mediaTypes=Video"));
    }

    #[test]
    fn auth_bodies() {
        let r = authenticate_by_name("alice", "p\"w");
        assert_eq!(r.method, Method::Post);
        let v: serde_json::Value = serde_json::from_str(r.body.as_deref().unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"Username": "alice", "Pw": "p\"w"}));
        let r = authenticate_quick_connect("s3cr3t");
        assert_eq!(r.body.as_deref(), Some(r#"{"Secret":"s3cr3t"}"#));
        assert_eq!(quick_connect_state("a b").path, "/QuickConnect/Connect?secret=a%20b");
    }

    #[test]
    fn library_queries() {
        let q = ItemsQuery {
            parent_id: Some("lib".into()),
            include_types: Some("Movie".into()),
            unplayed: true,
            favorites: true,
            genre: Some("Sci-Fi".into()),
            start: 200,
            limit: 100,
            ..Default::default()
        };
        let p = items("u", &q).path;
        assert!(p.starts_with("/Items?userId=u&parentId=lib&recursive=true&includeItemTypes=Movie&sortBy=SortName&sortOrder=Ascending&filters=IsUnplayed%2CIsFavorite&genres=Sci-Fi&startIndex=200&limit=100"), "{p}");
        assert!(p.ends_with("enableTotalRecordCount=true"), "{p}");
        let idx = ItemsQuery { name_less_than: Some("M".into()), ..Default::default() };
        assert!(items("u", &idx).path.contains("nameLessThan=M&startIndex=0&limit=0"));
        assert_eq!(set_favorite("u", "i", false).method, Method::Delete);
        assert_eq!(set_played("u", "i", true).path, "/UserPlayedItems/i?userId=u");
        assert_eq!(stream_path("i", "m"), "/Videos/i/stream?static=true&mediaSourceId=m");
    }

    #[test]
    fn image_paths_are_sized_and_tagged() {
        assert_eq!(
            image_path("abc", ImageType::Primary, "t1", 200, 300),
            "/Items/abc/Images/Primary?tag=t1&maxWidth=200&maxHeight=300&quality=90&format=Webp"
        );
    }
}
