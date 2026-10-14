# Jellyroll Remote: Android app design specification

A phone remote control for **jellyroll**, a TV app for Jellyfin that runs
full-screen on a Linux HTPC. This document is self-contained: it is meant
to be handed to an AI coding assistant (e.g. Gemini in Android Studio)
together with the instruction "build this app, milestone by milestone".

---

## 0. Instructions for the assistant building this

- Build the milestones in section 11 in order. Finish, compile and run
  each one before starting the next. Show me what changed after each.
- **Do not assume versions.** Use the versions the current Android Studio
  "Empty Activity (Compose)" template produces, and the current stable
  release of any library you add. Don't downgrade anything the template
  chose. If you're unsure whether an API is current, say so.
- Only add the libraries listed in section 3. Ask before adding others.
- Every network call in this document is real Jellyfin API (server
  version 10.10 or newer; tested target 12.2). Use the exact paths,
  parameters and JSON field names given here. Jellyfin field names are
  PascalCase in JSON.
- Don't invent jellyroll features. Section 6 lists exactly what jellyroll
  responds to; anything else is silently ignored by it.

---

## 1. What the app does

Jellyroll on the TV is a Jellyfin client. When it is signed in, it keeps
a WebSocket open to the Jellyfin server and tells the server it can be
remote-controlled. Any other Jellyfin client signed in as the same user
can then send it commands **through the server**.

Jellyroll Remote is such a client: a phone app that

1. signs in to the same Jellyfin server,
2. finds the jellyroll session(s) on it,
3. sends navigation, playback and text commands to the chosen one, and
4. shows what is playing (title, artwork, position, paused) and lets the
   user seek, pause, stop and change audio/subtitle tracks,
5. lets the user pick something on the phone and start it on the TV.

```
 Phone (Jellyroll Remote)  --HTTPS-->  Jellyfin server  --WebSocket-->  jellyroll (TV)
        ^                                    |
        +---------- session state -----------+   (GET /Sessions, polled)
```

No direct phone-to-TV connection, no changes to jellyroll, no pairing:
signing in to the same server as the same user is the pairing.

### Goals
- Instant, reliable D-pad control that feels like a physical remote.
- A "Now playing" view that is correct within a second or two.
- Works with any number of jellyroll TVs on the server (pick one).
- Works on a phone in portrait; usable one-handed.

### Non-goals (v1)
- Playing video on the phone.
- Controlling non-jellyroll Jellyfin clients (they may work, but are not
  tested or designed for; see 5.3).
- Volume control (jellyroll does not mix audio; volume is on the TV or
  receiver).
- Play queues (jellyroll plays one item at a time, then its own
  "next episode").
- Tablets, Wear OS, Android TV, iOS.

---

## 2. Glossary

- **Server**: a Jellyfin server, base URL like `http://192.168.1.10:8096`
  or `https://jellyfin.example.com`.
- **Session**: one signed-in client on the server (the TV, this phone, a
  browser...). Has a string `Id`.
- **Target**: the jellyroll session the user is controlling.
- **Ticks**: Jellyfin time unit, 10,000,000 ticks = 1 second.
- **Stream index**: Jellyfin's index of an audio or subtitle stream within
  a media source (`MediaStream.Index`).

---

## 3. Platform and stack

| Concern | Choice |
|---|---|
| Language | Kotlin |
| UI | Jetpack Compose, Material 3, single Activity, Navigation Compose |
| Architecture | MVVM: `ViewModel` + `StateFlow`, a repository layer; manual dependency injection (one `AppContainer` object). No Hilt. |
| HTTP | OkHttp |
| JSON | kotlinx.serialization (`ignoreUnknownKeys = true`, `explicitNulls = false`) |
| Images | Coil (Compose) |
| Storage | Jetpack DataStore (Preferences) |
| Secrets | Access token encrypted with an Android Keystore AES/GCM key before it is written to DataStore. (Jetpack Security's `EncryptedSharedPreferences` has been deprecated; check before using it.) |
| Async | Kotlin coroutines; `viewModelScope` |
| minSdk | 26 |
| targetSdk / compileSdk | The latest stable SDK in Android Studio |
| Permissions | `INTERNET`, `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE`, `CHANGE_WIFI_MULTICAST_STATE` (LAN discovery) |

Application id: `com.example.jellyrollremote` (I will rename it). App
name: **Jellyroll Remote**.

Cleartext HTTP must be allowed (home servers are usually plain `http://`
on the LAN): add a network security config that permits cleartext
traffic. Don't restrict it to specific hosts; the server address is typed
by the user.

---

## 4. Talking to Jellyfin

### 4.1 Identity and the Authorization header

Every request carries this header (one line; values percent-encoded like
JavaScript's `encodeURIComponent`):

```
Authorization: MediaBrowser Client="Jellyroll Remote", Device="<device name>", DeviceId="<device id>", Version="<app versionName>", Token="<access token or empty>"
```

- `Device`: `Settings.Global.DEVICE_NAME` if readable, else
  `"${Build.MANUFACTURER} ${Build.MODEL}"`.
- `DeviceId`: a random UUID generated on first launch and stored forever
  (DataStore). Never the Android ID.
- `Token`: empty string before sign-in.

Implement this as an OkHttp interceptor that reads the current token from
the repository.

Timeouts: connect 5 s, read 15 s. Retry nothing automatically except
where this document says so.

### 4.2 Finding the server

Two ways, both on the "Add server" screen:

**Typed address.** Normalize what the user types:
- no scheme → prefix `http://`;
- `http://` with no port → append `:8096`;
- strip trailing `/`;
- keep any path (`https://host/jellyfin`).

Validate by calling `GET {base}/System/Info/Public` (no auth). Response
fields used: `ServerName`, `Version`, `Id`. Reject versions older than
10.10.0 with a clear message.

**LAN discovery.** Send the ASCII string `Who is JellyfinServer?` as a UDP
broadcast to `255.255.255.255:7359` (also to the Wi-Fi subnet's broadcast
address), from an ephemeral port. Listen for replies for 3 seconds. Each
reply is JSON:

```json
{ "Address": "http://192.168.1.10:8096", "Id": "…", "Name": "Living room server", "EndpointAddress": null }
```

Hold a `WifiManager.MulticastLock` while listening (some phones drop
broadcast replies otherwise). Show found servers as a list; deduplicate by
`Id`. Do this on `Dispatchers.IO`.

### 4.3 Signing in

Two methods; password is the default tab.

**Username and password**
```
POST {base}/Users/AuthenticateByName
Content-Type: application/json
{ "Username": "alice", "Pw": "secret" }
```
Response (`AuthenticationResult`): `AccessToken`, `User.Id`, `User.Name`,
`ServerId`. HTTP 401 → "Wrong username or password."

Optionally show the server's public users as tappable chips above the
form: `GET {base}/Users/Public` → array of `{ "Name", "Id", "HasPassword" }`.

**Quick Connect** (sign in by approving a code from another signed-in
device):
1. `GET {base}/QuickConnect/Enabled` → `true`/`false`. If false, hide this
   tab.
2. `POST {base}/QuickConnect/Initiate` → `{ "Code": "123456", "Secret": "…" }`.
   Show the code large, with "Enter this code in Jellyfin on another device:
   profile picture → Quick Connect".
3. Poll `GET {base}/QuickConnect/Connect?secret={Secret}` every 3 s →
   `{ "Authenticated": true|false }`. On 401/404 the code expired: initiate
   again.
4. When authenticated: `POST {base}/Users/AuthenticateWithQuickConnect`
   with `{ "Secret": "…" }` → `AuthenticationResult` as above.

Store per server: base URL, server id, server name, user id, user name,
token (encrypted). Several servers/users may be stored; one is active.

**Sign out:** `POST {base}/Sessions/Logout` (revokes the token), then
delete it locally.

Any request answering **401** later means the token was revoked: drop it
and send the user to sign-in for that server, keeping the server.

### 4.4 Finding jellyroll sessions

```
GET {base}/Sessions?controllableByUserId={userId}&activeWithinSeconds=960
```
Returns an array of `SessionInfoDto`. Fields used:

| Field | Use |
|---|---|
| `Id` | target session id for all commands |
| `Client` | `"Jellyroll"` for jellyroll |
| `DeviceName` | the TV's name (jellyroll uses the HTPC's hostname) |
| `DeviceId` | to remember the chosen target across launches |
| `ApplicationVersion` | shown in the device picker |
| `SupportsRemoteControl` | must be `true` |
| `SupportedCommands` | array of `GeneralCommandType` names the client accepts |
| `NowPlayingItem` | `BaseItemDto` of what is playing, or absent |
| `PlayState` | `PositionTicks`, `IsPaused`, `CanSeek`, `AudioStreamIndex`, `SubtitleStreamIndex`, `MediaSourceId` |
| `LastActivityDate` | ISO date; for sorting |

Filtering:
- Drop the phone's own session (`DeviceId` equal to ours).
- Drop sessions with `SupportsRemoteControl != true`.
- Show `Client == "Jellyroll"` sessions first, with a TV icon. Other
  controllable clients go under an "Other devices" header, collapsed, with
  a note that they're not tested.

Remember the chosen target by `DeviceId` (session ids change when the TV
reconnects). On launch, and whenever the session list refreshes, re-find
the target by `DeviceId`.

### 4.5 Keeping state current

While the remote screen is visible (lifecycle `STARTED`), poll
`GET /Sessions?…` (4.4) **every 1 second**. Stop polling when the app goes
to the background. Back off to every 5 s after 3 consecutive network
failures, and show the connection banner (8.3).

Jellyroll reports its playback position to the server **every 10 seconds
while playing**, and immediately on pause/unpause, seek, track change and
stop. So between reports, **interpolate the position locally**:

```
shownPosition = lastReportedPositionTicks + (if !IsPaused) elapsedSinceReport
```

Reset the anchor whenever `PositionTicks` or `IsPaused` changes in a poll.
Clamp to the item's `RunTimeTicks`. After the user seeks, show the
requested position immediately (optimistic) and keep it until a poll
reports a position within 5 s of it, or 4 s pass.

> A later version may replace polling with the server's WebSocket
> (`/socket`, sending `{"MessageType":"SessionsStart","Data":"0,1000"}` to
> receive `Sessions` messages). Not in v1.

### 4.6 Commands

All commands are `POST`, need the auth header, have no response body and
answer **204** on success. Send them on a single-threaded queue so they
reach the server in the order pressed. Don't wait for a command's response
before sending the next one (the D-pad must stay responsive), but if one
fails with a network error, drop the rest of the queue and show the
connection banner.

**Navigation and general commands**
```
POST {base}/Sessions/{targetId}/Command
Content-Type: application/json
{ "Name": "MoveUp", "Arguments": {} }
```
`Arguments` values are always strings.

**Playback**
```
POST {base}/Sessions/{targetId}/Playing/{command}?seekPositionTicks={ticks}
```
`{command}` is one of `Stop`, `Pause`, `Unpause`, `PlayPause`, `Seek`,
`Rewind`, `FastForward`, `NextTrack`, `PreviousTrack`.
`seekPositionTicks` only with `Seek`.

**Play an item on the TV**
```
POST {base}/Sessions/{targetId}/Playing?playCommand=PlayNow&itemIds={itemId}&startPositionTicks={ticks}
```
Optional: `audioStreamIndex`, `subtitleStreamIndex` (jellyroll ignores
these two on remote Play; send them anyway, harmless).

**Show a message on the TV**
```
POST {base}/Sessions/{targetId}/Message
Content-Type: application/json
{ "Header": "", "Text": "Dinner is ready", "TimeoutMs": 5000 }
```

---

## 5. What jellyroll responds to

This is the complete list. Jellyroll ignores everything else.

### 5.1 General commands (`/Sessions/{id}/Command`)

| `Name` | `Arguments` | Jellyroll does |
|---|---|---|
| `MoveUp` `MoveDown` `MoveLeft` `MoveRight` | – | Arrow key |
| `Select` | – | OK / Enter |
| `Back` | – | Back. On the home screen with nothing to go back to, it opens jellyroll's power menu (restart / shut down). |
| `GoHome` | – | Home: back to the home screen; pressed again on the home screen, to its top |
| `GoToSearch` | – | Opens Search |
| `ToggleContextMenu` or `ToggleOsdMenu` | – | Menu key. During playback: the audio & subtitles menu |
| `ToggleStats` | – | Info key. During playback: technical overlay |
| `SendString` | `String`: text | Types the text into the focused text field (Search). There is no backspace command. |
| `DisplayMessage` | `Header`, `Text`, `TimeoutMs` | Shows a message (5 s default, 60 s max). Prefer the `/Message` endpoint, which the server turns into this. |
| `SetAudioStreamIndex` | `Index`: stream index | Switches the audio track while playing |
| `SetSubtitleStreamIndex` | `Index`: stream index, `-1` for off | Switches subtitles while playing |

**During playback** the same keys act on the player, as on the TV remote:

| Key | During playback |
|---|---|
| `MoveLeft` / `MoveRight` | Scrub back / forward (like `Rewind` / `FastForward`) |
| `Select` | Play/pause; or apply a scrub at once, skip the intro/credits when that button shows, start the next episode during its countdown |
| `MoveDown`, Menu | Audio & subtitles menu (then arrows move in it, `Select` picks, `Back` closes) |
| `Back` | Cancel a scrub or dismiss the next-episode card; otherwise **stop** |
| `MoveUp`, `GoHome`, `GoToSearch` | Nothing |

Jellyroll lists exactly these in its session's `SupportedCommands`. Only
show a control in the UI if its command is in the target's
`SupportedCommands` (so a future jellyroll that adds commands lights them
up, and older ones hide them).

### 5.2 Playback commands (`/Sessions/{id}/Playing/{command}`)

| Command | Jellyroll does |
|---|---|
| `PlayPause` | Toggle pause |
| `Pause`, `Unpause` | Set paused / playing |
| `Seek` + `seekPositionTicks` | Jump to that position |
| `Stop` | Stop; back to the page playback started from |
| `Rewind`, `FastForward` | Skip back / forward 10 s, applied 0.7 s after the last press; presses less than 0.45 s apart add up and speed up (30 s, then 60 s steps) |
| `NextTrack`, `PreviousTrack` | Next / previous **chapter** (not item) |

### 5.3 Play (`/Sessions/{id}/Playing`)

Only `playCommand=PlayNow`; plays the first item id (or `startIndex`).
`PlayNext`, `PlayLast` and `PlayInstantMix` are ignored. If something is
playing it stops (and reports progress) first. Note: for an episode the
server may expand `itemIds` to the rest of the season; jellyroll plays
the first one and its own next-episode feature continues from there.

### 5.4 Behaviour to design around

- **Commands are queued** in jellyroll (32 deep) and processed once per
  frame; more are dropped. Repeat-on-hold must not exceed ~8 commands/s.
- **Commands wait while a video is starting** (the TV may be switching
  refresh rate for a second or two). Don't treat the absence of a quick
  state change as failure.
- **Text entry**: `SendString` appends; the user can't delete on the TV
  from the phone. The keyboard sheet (7.4) therefore sends the whole text
  once, on "Send", not per keystroke.
- **Position** updates every 10 s while playing (interpolate, 4.5).
- **Remote control only works for the same user** the TV is signed in as
  (or an admin with remote-control permission). If no jellyroll session
  appears, the likely cause is a different user on the TV; say so in the
  empty state (7.2).

---

## 6. Data model (app side)

```kotlin
data class ServerAccount(
    val serverId: String, val serverName: String, val baseUrl: String,
    val userId: String, val userName: String, val token: String,   // token decrypted in memory only
)

data class Target(
    val sessionId: String, val deviceId: String, val deviceName: String,
    val client: String, val appVersion: String?,
    val supported: Set<String>,          // SupportedCommands
    val nowPlaying: NowPlaying?,
)

data class NowPlaying(
    val itemId: String, val name: String,
    val seriesName: String?, val seasonEpisode: String?,   // "S1:E3"
    val runTimeTicks: Long, val positionTicks: Long, val isPaused: Boolean,
    val canSeek: Boolean, val mediaSourceId: String?,
    val audioStreamIndex: Int?, val subtitleStreamIndex: Int?,
    val imageItemId: String, val imageTag: String?,        // see 7.3 artwork
)

data class Track(val index: Int, val label: String, val isDefault: Boolean)
```

Tracks for the current item: `GET {base}/Items/{itemId}?userId={userId}`
→ `MediaSources[]` (pick the one whose `Id` equals
`PlayState.MediaSourceId`, else the first) → `MediaStreams[]` where
`Type` is `"Audio"` or `"Subtitle"`. Label: `DisplayTitle`, else
`Language` + `Codec`. Cache per item id.

Only the DTO fields named in this document need Kotlin classes; mark
everything nullable with defaults (servers vary).

---

## 7. Screens

Dark theme only, matching jellyroll's TV interface:

| Token | Value |
|---|---|
| Background | `#0C0E14` |
| Surface (cards, buttons) | `#1C1E26` |
| Text | `#EBEBF0` |
| Secondary text | `#AAAFBE` |
| Accent (focus, progress, primary actions) | `#00A4DC` |
| Notice / warning | `#F0B43C` |

Rounded corners 16 dp on cards, fully round on remote buttons. Haptic
feedback (`HapticFeedbackType.TextHandleMove` or a short `VIBRATE` click)
on every remote button press. Respect system font scaling; minimum touch
target 56 dp on the remote.

### 7.1 Onboarding: Add server → Sign in

1. **Add server**: a text field ("Server address", hint
   `192.168.1.10 or jellyfin.example.com`), a **Connect** button, and
   below it **Find servers on this network** which runs discovery and lists
   results as tappable rows (name + address). Errors inline under the
   field: unreachable, not a Jellyfin server, version too old.
2. **Sign in** to *Server name*: tabs **Password** | **Quick Connect**.
   Password: public-user chips, username, password (show/hide), **Sign in**.
   Quick Connect: the code in 48 sp monospace digits, instructions,
   spinner, **New code**.

### 7.2 Choose a TV

Shown after sign-in and from the top bar of the remote. A list of
jellyroll sessions: TV icon, `DeviceName`, "Playing: <title>" or "Idle",
app version. Tap to select (becomes the target and opens the remote).
Pull to refresh.

Empty state: "No jellyroll found. Make sure jellyroll is running on your
TV and signed in as *userName* on *serverName*." with **Refresh**.

### 7.3 Remote (main screen)

Portrait layout, top to bottom:

1. **Top bar**: target `DeviceName` with a small status dot (green: the
   target is in the last poll; amber: polls failing; grey: target gone).
   Tap the name → Choose a TV. Overflow menu: Send message, Switch server
   / account, Settings.
2. **Now playing card** (only while `NowPlayingItem` exists; animated
   in/out):
   - Artwork 16:9 on the left (episode: the episode's `Primary` image;
     movie: `Backdrop` if available else `Primary`), via
     `GET {base}/Items/{id}/Images/{Primary|Backdrop}?maxWidth=480&quality=90&tag={tag}`.
     Image requests need no auth header.
   - Title (series name for episodes) and subtitle ("S1:E3 · Episode
     name" or the year).
   - Progress bar with a draggable thumb; elapsed and remaining time
     (`-1:02:13`). Dragging shows the target time; releasing sends `Seek`.
     Disabled if `CanSeek` is false.
   - Buttons: **⏮ chapter** (`PreviousTrack`), **−10 s** (`Rewind`),
     **Play/Pause** (large, `PlayPause`), **+10 s** (`FastForward`),
     **⏭ chapter** (`NextTrack`), **Stop**.
   - Chips: **Audio: English** and **Subtitles: Off**, each opening a
     bottom sheet listing the tracks (current one checked; subtitles list
     starts with "Off"). Picking sends `SetAudioStreamIndex` /
     `SetSubtitleStreamIndex` and updates the chip optimistically.
3. **D-pad**: a large circle (about 70% of screen width) split into four
   arcs (Up/Down/Left/Right) with **OK** in the centre. Press feedback:
   the arc darkens and haptics fire. **Press and hold** an arrow repeats
   it: first repeat after 400 ms, then every 125 ms (≤ 8/s, 5.4).
4. **Button row**: **Back**, **Home**, **Menu** (`ToggleContextMenu`).
5. **Button row**: **Search** (`GoToSearch`, then opens the keyboard sheet
   7.4), **Keyboard** (keyboard sheet only), **Info** (`ToggleStats`).

**Touchpad mode** (toggle in Settings, off by default): replaces the
D-pad with a touch surface. Swipe up/down/left/right (≥ 48 dp, mostly in
one axis) sends one Move per swipe; a long swipe sends one more Move for
every extra 64 dp; tap sends `Select`.

Hardware volume keys are **not** intercepted (they keep controlling the
phone's media volume).

### 7.4 Keyboard sheet

A bottom sheet with a single-line text field (auto-focused, keyboard
open) and **Send**. Send → `SendString` with the text, then clear the
field and close the sheet. Hint text: "Typed into jellyroll's search".

### 7.5 Play on TV (browse)

A second tab in a bottom navigation bar (**Remote** | **Browse**).
Sections, each a horizontal row of posters:

- **Continue Watching**: `GET {base}/UserItems/Resume?userId={u}&limit=20&mediaTypes=Video&enableImageTypes=Primary,Backdrop,Thumb&imageTypeLimit=1`
- **Next Up**: `GET {base}/Shows/NextUp?userId={u}&limit=20&enableImageTypes=Primary,Backdrop,Thumb&imageTypeLimit=1`
- **Search** field at the top: `GET {base}/Items?userId={u}&searchTerm={q}&recursive=true&includeItemTypes=Movie,Series,Episode&limit=40&enableImageTypes=Primary&imageTypeLimit=1` (debounce 300 ms).

Results are `BaseItemDtoQueryResult` → `Items[]` (`Id`, `Name`, `Type`,
`SeriesName`, `ParentIndexNumber`, `IndexNumber`, `ProductionYear`,
`RunTimeTicks`, `ImageTags`, `UserData.PlaybackPositionTicks`).

Tapping a movie or episode opens a sheet: title, overview (fetch
`/Items/{id}?userId={u}` for it), and **Play on <TV name>** /
**Resume from 0:42:10** (when `UserData.PlaybackPositionTicks > 0`) /
**Play from start**. Sends the Play command (4.6) and switches to the
Remote tab. Tapping a series opens its seasons
(`GET /Shows/{seriesId}/Seasons?userId={u}`) then episodes
(`GET /Shows/{seriesId}/Episodes?userId={u}&seasonId={id}`).

### 7.6 Send message

Dialog from the overflow menu: text field, duration chips (5 s / 15 s /
60 s), **Send** → `/Message` (4.6).

### 7.7 Settings

- Accounts: list of saved servers/users; add, switch, sign out.
- Touchpad mode (toggle).
- Keep screen on while the remote is open (toggle, default on).
- Haptics (toggle, default on).
- About: app version, server version, the target's jellyroll version.

---

## 8. Behaviour and errors

### 8.1 App start
- No account → Add server.
- Account, remembered target → Remote immediately (show the cached
  device name and a "Connecting…" dot until the first poll).
- Account, no target → Choose a TV.

### 8.2 Target disappears
If the target's `DeviceId` is missing from 3 consecutive polls, grey the
dot and show a non-blocking banner: "*TV name* isn't connected. It may be
off or restarting." with **Choose another TV**. Keep polling; when it
reappears, clear the banner and continue.

### 8.3 Network trouble
Any request failing with a network error: amber dot and banner "Can't
reach *server name*" (auto-hides on the next successful poll). A 401: go
to Sign in for that server. Never show a raw stack trace; log details
with `Log.w`.

### 8.4 Lifecycle
- Poll only while the remote or browse screen is visible.
- Keep the screen on while the remote is visible (setting 7.7).
- Survive rotation and process death: targets and accounts come from
  DataStore; UI state from `SavedStateHandle` where it matters (current
  tab, open sheet).

---

## 9. Security and privacy

- The access token is the only secret; never log it, never put it in a
  URL (the API takes it in the header).
- The password is never stored.
- No analytics, no crash reporting, no third-party network calls. The app
  talks only to the user's server (and UDP broadcast on the LAN for
  discovery).
- Allow user-installed CAs for HTTPS servers with self-signed
  certificates (network security config: trust `system` and `user`).

---

## 10. Testing

- **Unit tests** (JVM): URL normalization; Authorization header encoding
  (spaces → `%20`, quotes → `%22`, commas → `%2C`); position
  interpolation and the optimistic-seek rule; session filtering (own
  device removed, jellyroll first, target re-found by `DeviceId`); track
  label fallback; DTO parsing with unknown fields and missing fields.
- **Repository tests** against OkHttp `MockWebServer` for sign-in, Quick
  Connect polling, `/Sessions` parsing, and that every command hits the
  exact path, query and JSON body in section 4.6.
- **Compose UI tests**: D-pad buttons send the right command; hold-to-
  repeat respects the 125 ms interval; now-playing card appears/disappears
  with `NowPlayingItem`.
- **Manual end-to-end** with a real Jellyfin server and jellyroll on the
  TV, signed in as the same user: every control in section 5 visibly does
  what the table says.

---

## 11. Milestones

1. **Skeleton**: project from the Compose template, theme (section 7
   colours), navigation between empty Add server / Sign in / Choose a TV
   / Remote / Browse / Settings screens.
2. **Server and sign-in**: typed address + `System/Info/Public`
   validation; password sign-in; token storage (encrypted); sign out;
   401 handling. Unit tests for URL normalization and the header.
3. **LAN discovery and Quick Connect.**
4. **Sessions**: Choose a TV list with filtering, remembered target,
   1 s polling tied to lifecycle, status dot and banners (8.2, 8.3).
5. **Remote controls**: D-pad with hold-to-repeat, Back/Home/Menu/Search/
   Info, keyboard sheet, command queue, haptics; hide controls not in
   `SupportedCommands`.
6. **Now playing**: card, artwork, interpolated progress, seek, playback
   buttons, audio/subtitle sheets.
7. **Browse and Play on TV**: Continue Watching, Next Up, search, series
   → seasons → episodes, play/resume sheet.
8. **Polish**: send message, touchpad mode, settings, keep-screen-on,
   multiple accounts, empty/error states, accessibility labels on every
   icon button (`contentDescription`), tests from section 10.

Each milestone ends with: it compiles, the tests written so far pass, and
the feature works against a real server.

---

## 12. Acceptance criteria

- From a cold start with a saved account and target, the remote is usable
  (first command accepted) in under 2 seconds on Wi-Fi.
- A D-pad press is visible on the TV in under 300 ms on the LAN (server
  and TV permitting).
- Holding an arrow scrolls the TV smoothly and stops within one step of
  release.
- Pause, seek, stop, track changes and Play on TV all work as described in
  section 5, and the now-playing card reflects the change within 2 s.
- Losing the server or turning off the TV produces the banners in 8.2/8.3,
  never a crash, and recovers on its own.
- No token, password or server response body appears in logcat.

---

## Appendix A: Example requests

```
GET http://192.168.1.10:8096/Sessions?controllableByUserId=5e7a1c0de5e7a1c0de5e7a1c0de5e7a1&activeWithinSeconds=960
Authorization: MediaBrowser Client="Jellyroll%20Remote", Device="Pixel%209", DeviceId="3f6b1c2e-…", Version="1.0.0", Token="abc123"

POST http://192.168.1.10:8096/Sessions/8d1f…/Command
Content-Type: application/json
{"Name":"MoveDown","Arguments":{}}

POST http://192.168.1.10:8096/Sessions/8d1f…/Command
Content-Type: application/json
{"Name":"SetSubtitleStreamIndex","Arguments":{"Index":"-1"}}

POST http://192.168.1.10:8096/Sessions/8d1f…/Playing/Seek?seekPositionTicks=36000000000

POST http://192.168.1.10:8096/Sessions/8d1f…/Playing?playCommand=PlayNow&itemIds=f3a9…&startPositionTicks=0
```

## Appendix B: Abridged `/Sessions` entry for jellyroll

```json
{
  "Id": "8d1f0c…",
  "Client": "Jellyroll",
  "DeviceName": "htpc",
  "DeviceId": "32a31291cae311915bb95c974c8a8796",
  "ApplicationVersion": "0.1.0",
  "SupportsRemoteControl": true,
  "SupportedCommands": ["MoveUp","MoveDown","MoveLeft","MoveRight","Select","Back","GoHome",
    "GoToSearch","ToggleContextMenu","ToggleOsdMenu","ToggleStats","SendString",
    "DisplayMessage","SetAudioStreamIndex","SetSubtitleStreamIndex"],
  "PlayState": { "PositionTicks": 25830000000, "CanSeek": true, "IsPaused": false,
                 "AudioStreamIndex": 1, "SubtitleStreamIndex": -1, "MediaSourceId": "f3a9…" },
  "NowPlayingItem": { "Id": "f3a9…", "Name": "Arrival", "Type": "Movie", "ProductionYear": 2016,
                      "RunTimeTicks": 69780000000, "ImageTags": { "Primary": "a1b2…" } }
}
```
(`SubtitleStreamIndex` may be absent rather than `-1` when subtitles are off; treat both as off.)
