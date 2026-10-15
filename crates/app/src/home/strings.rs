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

pub const SORT_NAME: &str = "Name";
pub const SORT_ADDED: &str = "Date added";
pub const SORT_YEAR: &str = "Year";
pub const SORT_RATING: &str = "Rating";
pub const FILTER_UNWATCHED: &str = "Unwatched";
pub const FILTER_FAVORITES: &str = "Favorites";
pub const GENRE_ALL: &str = "Genre: All";
pub const YEAR_ALL: &str = "Year: All";
pub const YEAR_LABEL: &str = "Year: %ss";
pub const YEAR_TITLE: &str = "Year";
pub const ALL_YEARS: &str = "All years";
pub const VIDEO_ALL: &str = "Video: All";
pub const VIDEO_LABEL: &str = "Video: %s";
pub const VIDEO_TITLE: &str = "Video";
pub const ALL_VIDEO: &str = "All video";
pub const VIDEO_SD: &str = "SD";
pub const VIDEO_HD: &str = "HD";
pub const VIDEO_4K: &str = "4K";
pub const VIDEO_3D: &str = "3D";
pub const VIDEO_BLURAY: &str = "Blu-ray";
pub const VIDEO_DVD: &str = "DVD";
pub const DECADE: &str = "%ss";
pub const YEAR_ONE_LABEL: &str = "Year: %s";
pub const ALL_OF_DECADE: &str = "All of the %ss";
pub const GENRE_PREFIX: &str = "Genre: ";

pub const PLAY: &str = "Play";
pub const WATCHED: &str = "Watched";
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
pub const FILMOGRAPHY: &str = "Movies & shows";
pub const IN_PLAYLIST: &str = "In this playlist";
pub const IN_COLLECTION: &str = "In this collection";
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
pub const SWITCH_USER: &str = "Switch user";
pub const WHOS_WATCHING: &str = "Who's watching?";
pub const ADD_USER: &str = "Add user";
pub const ADD_SERVER: &str = "Add server";
pub const ABOUT: &str = "About this device";
pub const DISPLAY: &str = "Display";
pub const UI_RES_1080: &str = "Interface: 1080p, upscaled (smoother)";
pub const UI_RES_NATIVE: &str = "Interface: full resolution (sharpest)";
pub const MATCH_REFRESH: &str = "Match refresh rate";
pub const SORT_TITLE: &str = "Sort by";
pub const GENRE_TITLE: &str = "Genre";
pub const REVERSE_ORDER: &str = "Reverse order";
pub const SORT_LABEL: &str = "Sort: %s";
pub const SORT_REVERSED: &str = "Sort: %s, reversed";
pub const CLEAR_ALL: &str = "Clear all";
pub const GENRES_MORE: &str = "Genres: %s +%s";
pub const UI_RES_TITLE: &str = "Interface resolution";
pub const UI_RES_OPT_NATIVE: &str = "Full resolution (sharpest)";
pub const UI_RES_OPT_1080: &str = "1080p, upscaled (smoother)";
pub const AUDIO_OUTPUT_TITLE: &str = "Audio output";
pub const CHANNELS_TITLE: &str = "Channels for decoded audio";
pub const DISPLAY_HINT: &str = "Select to change. Matching plays each video at its own frame rate (films at 24 Hz); set for %s.";
pub const AUDIO_OUTPUT: &str = "Output: %s";
pub const AUDIO_AUTO: &str = "automatic";
pub const PASSTHROUGH: &str = "Passthrough";
pub const CHANNELS_LABEL: &str = "Channels: %s";
pub const CHANNELS_AUTO: &str = "auto";
pub const CHANNELS_STEREO: &str = "stereo";
pub const AUDIO_HINT: &str = "Passthrough (set per output) sends Dolby and DTS audio undecoded, \
     for a receiver that decodes them. Applies from the next video.";

pub const PLAYBACK: &str = "Playback";
pub const SCREENSAVER_LABEL: &str = "Screensaver: %s";
pub const SCREENSAVER_TITLE: &str = "Screensaver";
pub const SCREENSAVER_OFF: &str = "Off";
pub const MINUTES: &str = "%s minutes";
pub const SMOOTHING: &str = "Motion smoothing";
pub const DEINTERLACE_LABEL: &str = "Deinterlacing: %s";
pub const DEINTERLACE_TITLE: &str = "Deinterlacing";
pub const DEINTERLACE_AUTO: &str = "Automatic";
pub const DEINTERLACE_AUTO_LONG: &str = "Automatic (interlaced videos only)";
pub const DEINTERLACE_ALWAYS: &str = "Always";
pub const DEINTERLACE_OFF: &str = "Off";
pub const TONE_LABEL: &str = "HDR tone mapping: %s";
pub const TONE_TITLE: &str = "HDR tone mapping";
pub const TONE_AUTO: &str = "Automatic";
pub const PLAYBACK_HINT: &str = "Motion smoothing blends frames for smoother pans when the TV can't \
     match a video's frame rate; some find it looks artificial. Tone mapping applies to HDR videos \
     on an SDR screen. Applies from the next video.";
pub const STORAGE: &str = "Storage";
pub const IMAGE_CACHE_LABEL: &str = "Artwork cache: %s";
pub const IMAGE_CACHE_TITLE: &str = "Artwork cache size";
pub const CLEAR_CACHE: &str = "Clear cache";
pub const STORAGE_HINT: &str = "Artwork uses %s of %s on disk. Clearing removes downloaded artwork \
     and saved pages; they load again from the server.";
pub const STORAGE_HINT_UNKNOWN: &str = "Clearing removes downloaded artwork and saved pages; they \
     load again from the server.";

pub const POWER_TITLE: &str = "Power";
pub const EXIT_APP: &str = "Exit to console";
pub const REBOOT: &str = "Restart";
pub const POWER_OFF: &str = "Shut down";
pub const CANCEL: &str = "Cancel";
pub const CANT_PLAY_DIRECTLY: &str = "This video can't play directly";
pub const NO_HW_DECODE_VIDEO: &str = "This computer's graphics can't decode %s video.";
pub const SOFTWARE_4K_NOTE: &str =
    "Software decoding 4K video will probably stutter on this computer.";
pub const NO_DIRECT_PLAY: &str = "The server can't send this file as it is.";
pub const PLAY_SOFTWARE: &str = "Software decode";
pub const PLAY_TRANSCODE: &str = "Server transcode";
pub const RESTARTING: &str = "Restarting…";
pub const SHUTTING_DOWN: &str = "Shutting down…";
pub const POWER_NOT_ALLOWED: &str =
    "This user may not do that. Install the polkit rule from docs/polkit-jellyroll.rules.";
pub const POWER_FAILED: &str = "That didn't work: %s";

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

pub const DEBUG_INFO: &str = "Debug info";
pub const DEBUG_TITLE: &str = "Debug information";
pub const DEBUG_CONFIRM: &str =
    "Write everything needed to diagnose playback problems with %s to this file?";
pub const DEBUG_TAKES: &str = "It takes about 15 seconds: the item plays for a few seconds without picture or sound. Access tokens are left out.";
pub const DEBUG_WRITE: &str = "Write";
pub const DEBUG_WORKING: &str = "Collecting… (about 15 seconds)";
pub const DEBUG_DONE: &str = "Written to:";
pub const DEBUG_READ_HINT: &str = "Over SSH: sudo cat <that path>";
pub const DEBUG_FAILED: &str = "Couldn't write it: %s";
pub const OK: &str = "OK";

/// Every string, for the glyph-coverage test.
#[cfg(test)]
pub const ALL: &[&str] = &[
    DEBUG_INFO,
    DEBUG_TITLE,
    DEBUG_CONFIRM,
    DEBUG_TAKES,
    DEBUG_WRITE,
    DEBUG_WORKING,
    DEBUG_DONE,
    DEBUG_READ_HINT,
    DEBUG_FAILED,
    OK,
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
    YEAR_ALL,
    YEAR_LABEL,
    YEAR_TITLE,
    ALL_YEARS,
    VIDEO_ALL,
    VIDEO_LABEL,
    VIDEO_TITLE,
    ALL_VIDEO,
    VIDEO_SD,
    VIDEO_HD,
    VIDEO_4K,
    VIDEO_3D,
    VIDEO_BLURAY,
    VIDEO_DVD,
    DECADE,
    YEAR_ONE_LABEL,
    ALL_OF_DECADE,
    GENRE_PREFIX,
    PLAY,
    WATCHED,
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
    FILMOGRAPHY,
    IN_PLAYLIST,
    IN_COLLECTION,
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
    SWITCH_USER,
    WHOS_WATCHING,
    ADD_USER,
    ADD_SERVER,
    ABOUT,
    DISPLAY,
    UI_RES_1080,
    UI_RES_NATIVE,
    MATCH_REFRESH,
    SORT_TITLE,
    GENRE_TITLE,
    REVERSE_ORDER,
    SORT_LABEL,
    SORT_REVERSED,
    CLEAR_ALL,
    GENRES_MORE,
    UI_RES_TITLE,
    UI_RES_OPT_NATIVE,
    UI_RES_OPT_1080,
    AUDIO_OUTPUT_TITLE,
    CHANNELS_TITLE,
    "  ›",
    DISPLAY_HINT,
    AUDIO_OUTPUT,
    AUDIO_AUTO,
    PASSTHROUGH,
    CHANNELS_LABEL,
    CHANNELS_AUTO,
    CHANNELS_STEREO,
    AUDIO_HINT,
    PLAYBACK,
    SCREENSAVER_LABEL,
    SCREENSAVER_TITLE,
    SCREENSAVER_OFF,
    MINUTES,
    SMOOTHING,
    DEINTERLACE_LABEL,
    DEINTERLACE_TITLE,
    DEINTERLACE_AUTO,
    DEINTERLACE_AUTO_LONG,
    DEINTERLACE_ALWAYS,
    DEINTERLACE_OFF,
    TONE_LABEL,
    TONE_TITLE,
    TONE_AUTO,
    PLAYBACK_HINT,
    STORAGE,
    IMAGE_CACHE_LABEL,
    IMAGE_CACHE_TITLE,
    CLEAR_CACHE,
    STORAGE_HINT,
    STORAGE_HINT_UNKNOWN,
    POWER_TITLE,
    EXIT_APP,
    REBOOT,
    POWER_OFF,
    CANCEL,
    CANT_PLAY_DIRECTLY,
    NO_HW_DECODE_VIDEO,
    SOFTWARE_4K_NOTE,
    NO_DIRECT_PLAY,
    PLAY_SOFTWARE,
    PLAY_TRANSCODE,
    RESTARTING,
    SHUTTING_DOWN,
    POWER_NOT_ALLOWED,
    POWER_FAILED,
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
