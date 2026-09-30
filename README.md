# ricercar-subsonic

A [ricercar](https://github.com/ricercar-player/ricercar) source plugin that plays the
music of your own Subsonic-compatible server:
[Navidrome](https://www.navidrome.org), [Gonic](https://github.com/sentriz/gonic),
Airsonic-Advanced, [LMS](https://github.com/epoupon/lms), Ampache (Subsonic
mode) and the other servers of the [OpenSubsonic](https://opensubsonic.netlify.app)
family.

- **Browse:** recently added, albums, artists, playlists, starred items and
  most played.
- **Library:** your server's albums, artists and tracks join ricercar's own
  Albums, Artists and Tracks pages and its search, marked *Subsonic*; its
  playlists join the sidebar's Playlists.
- **Home:** shelves of recently added, recently played, most played and
  random albums on ricercar's Home page.
- **Search:** artists, albums, tracks and playlists.
- **Bit-perfect:** tracks play from the original file, byte for byte
  (`stream?format=raw`, seekable). Only when your DAC cannot take a file's
  sample rate or bit depth does the plugin ask the server for FLAC at the
  closest rate the DAC accepts (same 44.1/48 kHz family, never higher than
  the original). That needs the OpenSubsonic `transcoding` extension
  (Navidrome 0.64 and later); other servers cannot resample, and such a file
  is reported as unavailable on that output.
- **Stars** sync both ways, and plays are scrobbled to the server (its "now
  playing", play counts and "most played").
- **Loudness:** the ReplayGain tags the server reads (OpenSubsonic
  `replayGain`) are passed to ricercar.
- **Lyrics:** synced lyrics when the server has them (OpenSubsonic
  `songLyrics`), else plain ones (`getLyrics`).
- **Details and related music:** artist biographies, similar artists and
  top tracks, album notes and facts (label, genres, release type, dates),
  "Go to album / artist" from any track, radios from a track or an artist
  (`getSimilarSongs`, `getSimilarSongs2`), and continuous playback when
  ricercar's queue runs out. Most servers build these from Last.fm: without
  it, the lists may be empty.
- **Playlists:** create, rename and delete your own playlists, add and
  remove tracks; reorder them on Navidrome and Gonic. Playlists of other
  users and smart playlists stay read-only.
- **Settings**, where ricercar offers plugin settings: turn off play reporting, or keep
  original files only (a file the DAC cannot take is then skipped).

The plugin talks to the documented Subsonic / OpenSubsonic API only. It needs
API version 1.13 or later (token authentication); tested with Navidrome 0.64.

## Install

**From ricercar (0.4.0 and later):** open **Plugins** in the sidebar and
install *Subsonic*.

**By hand:** download `subsonic-x86_64` or `subsonic-aarch64` from the
[releases](https://github.com/ricercar-player/ricercar-subsonic/releases), check it
against its `.sha256` file, make it executable, and declare it in
`~/.config/ricercar/config.toml`:

```toml
[[plugins]]
id = "subsonic"
command = "/home/you/.local/bin/subsonic-x86_64"
# args = ["--server", "http://192.168.1.10:4533"]   # prefills the sign-in page
```

**From source:**

```sh
cargo build --release
# target/release/ricercar-subsonic
```

## Sign in

Click **Sign in** next to Subsonic in ricercar. A page opens in your browser
(served by the plugin on 127.0.0.1): enter the server address, then your
user name and password, or an API key if your server offers them
(OpenSubsonic `apiKeyAuthentication`).

The password goes from your browser to the plugin to your server; ricercar
never sees it. The plugin keeps a salted token instead (`md5(password +
salt)` and the salt, as the Subsonic API defines it), or the API key, in
`~/.local/share/ricercar/plugins/subsonic/auth.json` (mode 600). Changing
your password on the server revokes it.

A few servers refuse tokens (those that check passwords against LDAP); the
plugin then falls back to the Subsonic `p=enc:` form and has to keep the
password itself, hex-encoded, in the same file. It says so in ricercar's log.

Signing in from another computer than the one running ricercar: paste
`<server address> <API key>` in ricercar's sign-in field.

## Notes

- Stream **and cover** URLs carry the credentials as query parameters, as
  every Subsonic client does. Stream URLs are never stored; cover URLs are
  part of item metadata, so they can end up in ricercar's saved queue and
  playlists. Use an API key or a dedicated user if that matters to you.
- Transcoded streams have no known length, so they cannot be seeked.
  Originals can.
- The server address is fixed at sign-in. To use another server, sign out
  and in again. To use two servers at once, declare the plugin twice with
  different `id`s.
- Use `https://` for a server outside your home network.
- Playlists are those the server shows you: yours and the ones others
  share. Subsonic's `search3` does not search playlists; the plugin matches
  their names itself.
- ricercar's log never gets your user name or credentials: errors are
  stripped of the `u`, `t`, `s`, `p` and `apiKey` parameters.
- The Tracks list uses an empty `search3` query, which OpenSubsonic servers
  answer with every song; some older servers return nothing there.

## Protocol

Plugin protocol 1, as described in ricercar's
[docs/plugins.md](https://github.com/ricercar-player/ricercar/blob/main/docs/plugins.md),
with the `library`, `lyrics`, `details`, `radio` and `playlist_edit`
capabilities and plugin settings. Older ricercar versions ignore what they
do not know.

| Ref | Meaning |
|---|---|
| `recent`, `albums`, `artists`, `playlists`, `favorites`, `frequent` | Top-level sections |
| `recent`, `played`, `frequent`, `random` | Home shelves (albums) |
| `t/<id>` | Track (song) |
| `a/<id>` | Album |
| `r/<id>` | Artist (its albums) |
| `p/<id>` | Playlist (its tracks carry `entry_id` = `<position>:<song id>`) |
| `rt/<id>`, `rr/<id>` | Radio of a track, of an artist |
| `top/<id>`, `sim/<id>` | Top tracks, similar artists of an artist |

Error codes follow the protocol: refused credentials mark the session
expired (`auth_required`) and send `auth.changed`; missing items answer
`not_found`; files the output cannot take answer `unavailable`; unreachable
servers answer `network`.

## Development

```sh
cargo test
cargo clippy --all-targets
tests/navidrome.sh     # end to end against a throwaway Navidrome (docker, ffmpeg)
```

The CI builds static binaries (musl) for x86_64 and aarch64 on every tag
`v*` and attaches them, with their SHA-256, to a GitHub release.
`contrib/hub-entry.toml` is the entry for the
[ricercar plugin hub](https://github.com/ricercar-player/ricercar-plugins).

## Licence

MIT. Subsonic, Navidrome and the other server names are trademarks of their
owners; this plugin is not affiliated with any of them.
