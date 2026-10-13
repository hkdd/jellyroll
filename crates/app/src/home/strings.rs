//! User-visible strings of the browsing UI (spec 10.4: a string table, no
//! literals in widgets). English only for now; a translation swaps this
//! module's values. Every character must exist in the bundled fonts (a test
//! in `home::tests` checks).

pub const NAV_HOME: &str = "Home";
pub const NAV_SEARCH: &str = "Search";
pub const NAV_SETTINGS: &str = "Settings";

pub const ITEMS: &str = "items";
pub const LOADING: &str = "Loading…";
pub const NOTHING_HERE: &str = "Nothing here yet.";
pub const NO_MATCHES: &str = "Nothing matches these filters.";

pub const SORT_NAME: &str = "Sort: Name";
pub const SORT_ADDED: &str = "Sort: Date added";
pub const SORT_YEAR: &str = "Sort: Year";
pub const SORT_RATING: &str = "Sort: Rating";
pub const FILTER_UNWATCHED: &str = "Unwatched";
pub const FILTER_FAVORITES: &str = "Favorites";
pub const GENRE_ALL: &str = "Genre: All";
pub const GENRE_PREFIX: &str = "Genre: ";

pub const PLAY: &str = "Play";
pub const RESUME_PREFIX: &str = "Resume ";
pub const PLAY_FROM_START: &str = "From the start";
pub const MARK_WATCHED: &str = "Mark watched";
pub const MARK_UNWATCHED: &str = "Mark unwatched";
pub const ADD_FAVORITE: &str = "Add to favorites";
pub const REMOVE_FAVORITE: &str = "Remove from favorites";
pub const SEASONS: &str = "Seasons";
pub const EPISODES: &str = "Episodes";
pub const CAST: &str = "Cast & crew";
pub const MORE_LIKE_THIS: &str = "More like this";
pub const NO_HW_DECODE: &str = "This device can't decode %s in hardware: it would play in software, \
     if at all (see Settings for what this GPU decodes).";

pub const SEARCH_TITLE: &str = "Search";
pub const SEARCH_HINT: &str = "Type to search movies, shows and episodes.";
pub const SEARCH_MOVIES: &str = "Movies";
pub const SEARCH_SHOWS: &str = "Shows";
pub const SEARCH_EPISODES: &str = "Episodes";
pub const SEARCH_NONE: &str = "No results.";
pub const KEY_SPACE: &str = "Space";
pub const KEY_DELETE: &str = "Delete";
pub const KEY_CLEAR: &str = "Clear";

pub const SETTINGS_TITLE: &str = "Settings";
pub const ACCOUNT: &str = "Account";
pub const SIGNED_IN_AS: &str = "Signed in as %s on %s";
pub const SIGN_OUT: &str = "Sign out";
pub const CHANGE_SERVER: &str = "Change server";
pub const ABOUT: &str = "About this device";
pub const DISPLAY: &str = "Display";
pub const UI_RES_1080: &str = "Interface: 1080p, upscaled (smoother)";
pub const UI_RES_NATIVE: &str = "Interface: full resolution (sharpest)";
pub const UI_RES_HINT: &str =
    "Select to switch. Video always plays at the display's full resolution.";

pub const SIGN_IN_PASSWORD: &str = "Sign in with password";
pub const NEW_CODE: &str = "New code";
pub const USERNAME: &str = "Username";
pub const PASSWORD: &str = "Password";
pub const SIGN_IN: &str = "Sign in";
pub const SIGNING_IN: &str = "Signing in…";
pub const USERS_ON_SERVER: &str = "Users on this server";

pub const PLAYER_HINT: &str =
    "Left/Right: seek    Down: audio & subtitles    Info: details    Back: stop";
pub const PAUSED: &str = "Paused";
pub const BUFFERING: &str = "Buffering…";
pub const AUDIO: &str = "Audio";
pub const SUBTITLES: &str = "Subtitles";
pub const SUBTITLES_OFF: &str = "Off";
pub const SKIP_INTRO: &str = "Skip intro";
pub const SKIP_RECAP: &str = "Skip recap";
pub const SKIP_PREVIEW: &str = "Skip preview";
pub const SKIP_COMMERCIAL: &str = "Skip ad";
pub const SKIP_CREDITS: &str = "Skip credits";
pub const UP_NEXT: &str = "Up next";
pub const PLAY_NOW_HINT: &str = "Select: play now    Back: cancel";
pub const PLAYING_IN: &str = "Playing in %s";
pub const STARTING: &str = "Starting…";
pub const SOFTWARE_DECODE: &str =
    "Software decoding: this device has no hardware decoder for this video";

/// `%s` substitution, in order.
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut args = args.iter();
    let mut parts = template.split("%s");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for p in parts {
        out.push_str(args.next().copied().unwrap_or(""));
        out.push_str(p);
    }
    out
}

/// Every string, for the glyph-coverage test.
#[cfg(test)]
pub const ALL: &[&str] = &[
    NAV_HOME,
    NAV_SEARCH,
    NAV_SETTINGS,
    ITEMS,
    LOADING,
    NOTHING_HERE,
    NO_MATCHES,
    SORT_NAME,
    SORT_ADDED,
    SORT_YEAR,
    SORT_RATING,
    FILTER_UNWATCHED,
    FILTER_FAVORITES,
    GENRE_ALL,
    GENRE_PREFIX,
    PLAY,
    RESUME_PREFIX,
    PLAY_FROM_START,
    MARK_WATCHED,
    MARK_UNWATCHED,
    ADD_FAVORITE,
    REMOVE_FAVORITE,
    SEASONS,
    EPISODES,
    CAST,
    MORE_LIKE_THIS,
    NO_HW_DECODE,
    SEARCH_TITLE,
    SEARCH_HINT,
    SEARCH_MOVIES,
    SEARCH_SHOWS,
    SEARCH_EPISODES,
    SEARCH_NONE,
    KEY_SPACE,
    KEY_DELETE,
    KEY_CLEAR,
    SETTINGS_TITLE,
    ACCOUNT,
    SIGNED_IN_AS,
    SIGN_OUT,
    CHANGE_SERVER,
    ABOUT,
    DISPLAY,
    UI_RES_1080,
    UI_RES_NATIVE,
    UI_RES_HINT,
    SIGN_IN_PASSWORD,
    NEW_CODE,
    USERNAME,
    PASSWORD,
    SIGN_IN,
    SIGNING_IN,
    USERS_ON_SERVER,
    PLAYER_HINT,
    PAUSED,
    BUFFERING,
    AUDIO,
    SUBTITLES,
    SUBTITLES_OFF,
    SKIP_INTRO,
    SKIP_RECAP,
    SKIP_PREVIEW,
    SKIP_COMMERCIAL,
    SKIP_CREDITS,
    UP_NEXT,
    PLAY_NOW_HINT,
    PLAYING_IN,
    STARTING,
    SOFTWARE_DECODE,
];

#[cfg(test)]
mod tests {
    #[test]
    fn fill_substitutes_in_order() {
        assert_eq!(
            super::fill("Signed in as %s on %s", &["alice", "home"]),
            "Signed in as alice on home"
        );
        assert_eq!(super::fill("no args", &[]), "no args");
    }
}
