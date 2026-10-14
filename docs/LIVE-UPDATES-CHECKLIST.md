# Live updates and remote control: HTPC checklist

Built and tested against the mock server (D63). These steps check it
against your real Jellyfin server. You need jellyroll running on the TV
and the Jellyfin web app open on a phone or computer, signed in as the
same user.

Before starting, follow the log on the HTPC (from SSH):

```sh
sudo journalctl -u jellyroll -f | grep --line-buffered -E "live updates|remote control" | tee >(wl-copy)
```

(Running jellyroll from a console instead of the service? Use
`jellyroll 2>&1 | grep --line-buffered -E "live updates|remote control" | tee >(wl-copy)`.)
Within a few seconds of the home screen appearing you should see
`live updates connected`.

## 1. Changes made elsewhere

1. On the TV, open a movie's detail page.
2. In the web app, mark that movie watched (the check mark).
3. Within about 2 seconds the TV's Watched button turns on and the
   card shows its check mark, without pressing anything.
4. Favorite it in the web app: the TV's Favorite button follows.
5. Go back to the TV's home screen. Mark an episode in Continue Watching
   as watched in the web app: it leaves Continue Watching on the TV.

## 2. Remote control

In the web app, open the cast/"Play on" menu (the icon at the top
right). Jellyroll should be listed under the TV's device name.

1. Choose jellyroll, then play a movie from the web app: it starts on
   the TV.
2. Use the web app's remote controls: pause, play, seek on its progress
   bar, change the audio track and subtitles, stop. The TV follows each.
3. With nothing playing, use the web app's remote (arrows, OK, Back,
   Home): the TV's focus moves.
4. In the web app's remote screen, send a message: it appears on the TV
   for a few seconds (at the top while a video plays).

## 3. Reconnecting

1. Restart the Jellyfin server (Dashboard, Restart).
2. The log shows `live updates: server restarting; reconnecting in …`
   and then `live updates connected` again once the server is back.
3. Repeat step 1.2 to check updates arrive again.

Report back with the copied log lines and anything that didn't match.
