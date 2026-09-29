//! OpenSubsonic source plugin for ricercar (plugin protocol 1).
//!
//! Speaks JSON-RPC over stdin/stdout with the player, and the Subsonic API
//! with the user's server (Navidrome, Gonic, Airsonic-Advanced, LMS,
//! Ampache…). Tracks play from the original file, bit for bit
//! (`stream?format=raw`), unless the DAC cannot take its rate or depth; then
//! servers with the OpenSubsonic `transcoding` extension send FLAC at a rate
//! it does take.
//!
//! Options:
//!   --server URL   prefill the server address on the sign-in page

mod items;
mod login;
mod subsonic;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use items::{Output, Plan};
use subsonic::{Auth, Client, Error, Session};

const PROTOCOL: u64 = 1;
const PAGE: u64 = 200;
/// How long the artist index is reused between pages.
const ARTISTS_TTL: Duration = Duration::from_secs(300);

/// `<n>` random bytes from the kernel, as hex.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

struct RpcError {
    code: i64,
    message: String,
}

fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
    }
}

type Reply = Result<Value, RpcError>;

struct Out(Mutex<std::io::Stdout>);

impl Out {
    fn send(&self, v: Value) {
        let mut out = self.0.lock().unwrap();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

struct Plugin {
    out: Arc<Out>,
    server_hint: String,
    data_dir: Mutex<PathBuf>,
    french: Mutex<bool>,
    output: Mutex<Output>,
    client: Arc<Client>,
    session: Mutex<Option<Session>>,
    /// The credentials were refused: signed in, but they need renewing.
    expired: Mutex<bool>,
    login: Mutex<Option<login::Login>>,
    /// The whole artist index (`getArtists` has no paging), for a while.
    artists: Mutex<Option<(Instant, Vec<Value>)>>,
    /// When each playing track started, for the scrobble's `time`.
    started: Mutex<HashMap<String, u64>>,
}

impl Plugin {
    fn session(&self) -> Result<Session, RpcError> {
        let s = self.session.lock().unwrap().clone();
        match s {
            Some(s) if !*self.expired.lock().unwrap() => Ok(s),
            _ => Err(rpc_err(-32001, "sign in to your Subsonic server first")),
        }
    }

    fn fr(&self) -> bool {
        *self.french.lock().unwrap()
    }

    fn auth_path(&self) -> PathBuf {
        self.data_dir.lock().unwrap().join("auth.json")
    }

    fn auth_status(&self) -> Value {
        match &*self.session.lock().unwrap() {
            None => json!({"state": "signed_out"}),
            Some(s) => {
                let state = if *self.expired.lock().unwrap() {
                    "expired"
                } else {
                    "signed_in"
                };
                let detail = if s.server_name.is_empty() {
                    s.server.clone()
                } else {
                    format!("{} · {}", s.server_name, s.server)
                };
                let user = s.auth.user();
                let name = if user.is_empty() { &s.server } else { user };
                json!({"state": state, "account": {"display_name": name, "detail": detail}})
            }
        }
    }

    fn store(&self, s: Session) {
        let path = self.auth_path();
        let tmp = path.with_extension("tmp");
        let written = (|| {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(s.to_json().to_string().as_bytes())?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            eprintln!("cannot save the session in {}: {e}", path.display());
        }
        if matches!(s.auth, Auth::Password { .. }) {
            eprintln!("the server refuses tokens: the password is kept (hex) in auth.json");
        }
        eprintln!("signed in as {} on {}", s.auth.user(), s.server);
        *self.session.lock().unwrap() = Some(s);
        *self.expired.lock().unwrap() = false;
        *self.artists.lock().unwrap() = None;
        self.out.notify("auth.changed", self.auth_status());
    }

    /// Map a server failure; refused credentials mark the session expired.
    fn fail(&self, e: Error) -> RpcError {
        match e {
            Error::Auth | Error::Method(_) => {
                let was = std::mem::replace(&mut *self.expired.lock().unwrap(), true);
                if !was {
                    eprintln!("the server refused the credentials: {e}");
                    self.out.notify("auth.changed", self.auth_status());
                }
                rpc_err(-32001, "the server refused the stored credentials")
            }
            Error::NotFound => rpc_err(-32002, "not found on the server"),
            Error::Status(503, m) | Error::Status(429, m) => RpcError {
                code: -32004,
                message: m,
            },
            Error::Status(code, m) if code >= 500 => {
                rpc_err(-32005, format!("server error {code} {m}"))
            }
            Error::Status(code, m) => rpc_err(-32603, format!("server answered {code} {m}")),
            Error::Network(m) => rpc_err(-32005, m),
        }
    }

    fn get(&self, s: &Session, endpoint: &str, q: &[(&str, String)]) -> Reply {
        self.client.get(s, endpoint, q).map_err(|e| self.fail(e))
    }

    // ---------------------------------------------------------------- setup

    fn initialize(&self, p: &Value) -> Reply {
        let data_dir = p["data_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        *self.french.lock().unwrap() = p["locale"].as_str().is_some_and(|l| l.starts_with("fr"));
        *self.output.lock().unwrap() = Output::from_json(&p["output"]);
        *self.data_dir.lock().unwrap() = data_dir;
        *self.session.lock().unwrap() = std::fs::read_to_string(self.auth_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .and_then(|v| Session::from_json(&v));

        let proto = p["protocol"].as_u64().unwrap_or(0);
        if proto != PROTOCOL {
            eprintln!("host speaks protocol {proto}, this plugin {PROTOCOL}");
        }
        Ok(json!({
            "protocol": PROTOCOL,
            "plugin": {"id": "subsonic", "name": "Subsonic", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {
                "auth": true, "browse": true, "search": true, "resolve": true,
                "favorites": true, "reporting": true, "remote_control": false,
                "library": true
            }
        }))
    }

    // ----------------------------------------------------------------- auth

    fn auth_begin(self: &Arc<Self>) -> Reply {
        let french = self.fr();
        let mut login = self.login.lock().unwrap();
        if login.is_none() {
            let me = self.clone();
            let hint = self
                .session
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| s.server.clone())
                .unwrap_or_else(|| self.server_hint.clone());
            *login = Some(
                login::Login::start(
                    self.client.clone(),
                    Arc::new(move |s| me.store(s)),
                    hint,
                    french,
                )
                .map_err(|e| rpc_err(-32603, format!("cannot open the sign-in page: {e}")))?,
            );
        }
        let instructions = if french {
            "Saisissez l'adresse de votre serveur (Navidrome, Gonic, Airsonic…) dans la page qui s'ouvre, puis votre nom d'utilisateur et votre mot de passe. Depuis un autre appareil, collez ici « adresse clé-d'API » si votre serveur en fournit."
        } else {
            "Enter your server address (Navidrome, Gonic, Airsonic…) on the page that opens, then your user name and password. From another device, paste “address API-key” here if your server offers API keys."
        };
        Ok(json!({
            "url": login.as_ref().unwrap().url,
            "instructions": instructions,
            "expects_input": false
        }))
    }

    /// The paste field: `<server address> <API key>`, or nothing (the page
    /// may already have signed the user in).
    fn auth_complete(&self, p: &Value) -> Reply {
        let input = p["input"].as_str().unwrap_or("").trim();
        let mut words = input.split_whitespace();
        if let (Some(addr), Some(key), None) = (words.next(), words.next(), words.next()) {
            let server = subsonic::normalize_server(addr)
                .ok_or_else(|| rpc_err(-32602, "expected “server-address API-key”"))?;
            let auth = Auth::ApiKey {
                user: String::new(),
                key: key.to_string(),
            };
            match self.client.sign_in(&server, auth) {
                Ok(s) => self.store(s),
                Err(Error::Auth) => {}
                Err(e) => return Err(self.fail(e)),
            }
        }
        Ok(self.auth_status())
    }

    fn sign_out(&self) -> Reply {
        *self.session.lock().unwrap() = None;
        *self.expired.lock().unwrap() = false;
        *self.artists.lock().unwrap() = None;
        let _ = std::fs::remove_file(self.auth_path());
        Ok(Value::Null)
    }

    // --------------------------------------------------------------- browse

    fn root(&self) -> Reply {
        self.session()?;
        let fr = self.fr();
        let t = |en: &'static str, f: &'static str| if fr { f } else { en };
        let sections = [
            ("recent", t("Recently added", "Ajouts récents")),
            ("albums", t("Albums", "Albums")),
            ("artists", t("Artists", "Artistes")),
            ("playlists", t("Playlists", "Listes de lecture")),
            ("favorites", t("Starred", "Favoris")),
            ("frequent", t("Most played", "Les plus écoutés")),
        ];
        let sections: Vec<Value> = sections
            .iter()
            .map(
                |(r, title)| json!({"ref": r, "kind": "folder", "title": title, "browsable": true}),
            )
            .collect();
        Ok(json!({ "sections": sections }))
    }

    /// One page of `getAlbumList2` (no total: the server does not say).
    fn album_list(&self, s: &Session, kind: &str, offset: u64, limit: u64) -> Reply {
        let v = self.get(
            s,
            "getAlbumList2",
            &[
                ("type", kind.into()),
                ("size", limit.to_string()),
                ("offset", offset.to_string()),
            ],
        )?;
        let list = items::many(s, &v["albumList2"], "album", items::album);
        let has_more = list.len() as u64 == limit;
        Ok(json!({"items": list, "has_more": has_more}))
    }

    /// Every artist of the index, flattened, from the cache when fresh.
    fn all_artists(&self, s: &Session) -> Result<Vec<Value>, RpcError> {
        if let Some((at, list)) = &*self.artists.lock().unwrap()
            && at.elapsed() < ARTISTS_TTL
        {
            return Ok(list.clone());
        }
        let v = self.get(s, "getArtists", &[])?;
        let index = match &v["artists"]["index"] {
            Value::Array(a) => a.clone(),
            o @ Value::Object(_) => vec![o.clone()],
            _ => Vec::new(),
        };
        let list: Vec<Value> = index
            .iter()
            .flat_map(|i| items::many(s, i, "artist", items::artist))
            .collect();
        *self.artists.lock().unwrap() = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    fn list(&self, p: &Value) -> Reply {
        let s = self.session()?;
        let r = p["ref"].as_str().unwrap_or("");
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(PAGE).clamp(1, PAGE);
        let all = match r {
            "recent" => return self.album_list(&s, "newest", offset, limit),
            "albums" => return self.album_list(&s, "alphabeticalByName", offset, limit),
            "frequent" => return self.album_list(&s, "frequent", offset, limit),
            "artists" => self.all_artists(&s)?,
            "playlists" => {
                let v = self.get(&s, "getPlaylists", &[])?;
                items::many(&s, &v["playlists"], "playlist", items::playlist)
            }
            "favorites" => {
                let v = self.get(&s, "getStarred2", &[])?;
                let st = &v["starred2"];
                let mut all = items::many(&s, st, "artist", items::artist);
                all.extend(items::many(&s, st, "album", items::album));
                all.extend(items::many(&s, st, "song", items::song));
                all
            }
            _ => {
                let (kind, id) =
                    items::split_ref(r).ok_or_else(|| rpc_err(-32002, "no such list"))?;
                let q = [("id", id.to_string())];
                match kind {
                    "a" => {
                        let v = self.get(&s, "getAlbum", &q)?;
                        items::many(&s, &v["album"], "song", items::song)
                    }
                    "r" => {
                        let v = self.get(&s, "getArtist", &q)?;
                        let mut albums = match &v["artist"]["album"] {
                            Value::Array(a) => a.clone(),
                            o @ Value::Object(_) => vec![o.clone()],
                            _ => Vec::new(),
                        };
                        // Newest first, like a discography.
                        albums.sort_by_key(|a| std::cmp::Reverse(a["year"].as_i64().unwrap_or(0)));
                        albums.iter().filter_map(|a| items::album(&s, a)).collect()
                    }
                    "p" => {
                        let v = self.get(&s, "getPlaylist", &q)?;
                        items::many(&s, &v["playlist"], "entry", items::song)
                    }
                    _ => return Err(rpc_err(-32002, "no such list")),
                }
            }
        };
        Ok(page(all, offset, limit))
    }

    fn search(&self, p: &Value) -> Reply {
        let s = self.session()?;
        let query = p["query"].as_str().unwrap_or("").trim().to_string();
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(50).clamp(1, PAGE);
        let wanted: Vec<String> = p["kinds"]
            .as_array()
            .map(|k| {
                k.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| {
                ["artist", "album", "track", "playlist"]
                    .map(String::from)
                    .to_vec()
            });
        if query.is_empty() {
            return Ok(json!({ "groups": [] }));
        }
        let want = |k: &str| wanted.iter().any(|w| w == k);
        let count = |k: &str| if want(k) { limit } else { 0 }.to_string();
        let mut groups = Vec::new();
        if want("artist") || want("album") || want("track") {
            let v = self.get(
                &s,
                "search3",
                &[
                    ("query", query.clone()),
                    ("artistCount", count("artist")),
                    ("artistOffset", offset.to_string()),
                    ("albumCount", count("album")),
                    ("albumOffset", offset.to_string()),
                    ("songCount", count("track")),
                    ("songOffset", offset.to_string()),
                ],
            )?;
            let r = &v["searchResult3"];
            for (kind, key, f) in [
                (
                    "artist",
                    "artist",
                    items::artist as fn(&Session, &Value) -> _,
                ),
                ("album", "album", items::album),
                ("track", "song", items::song),
            ] {
                if want(kind) {
                    let found = items::many(&s, r, key, f);
                    let has_more = found.len() as u64 == limit;
                    groups.push(json!({"kind": kind, "items": found, "has_more": has_more}));
                }
            }
        }
        if want("playlist") {
            // search3 does not cover playlists: match their names here.
            let v = self.get(&s, "getPlaylists", &[])?;
            let needle = query.to_lowercase();
            let all: Vec<Value> = items::many(&s, &v["playlists"], "playlist", items::playlist)
                .into_iter()
                .filter(|p| {
                    p["title"]
                        .as_str()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
                })
                .collect();
            let mut g = page(all, offset, limit);
            g["kind"] = "playlist".into();
            groups.push(g);
        }
        Ok(json!({ "groups": groups }))
    }

    fn item_get(&self, p: &Value) -> Reply {
        let s = self.session()?;
        let (kind, id) = items::split_ref(p["ref"].as_str().unwrap_or(""))
            .ok_or_else(|| rpc_err(-32002, "no such item"))?;
        let q = [("id", id.to_string())];
        let it = match kind {
            "t" => items::song(&s, &self.get(&s, "getSong", &q)?["song"]),
            "a" => items::album(&s, &self.get(&s, "getAlbum", &q)?["album"]),
            "r" => items::artist(&s, &self.get(&s, "getArtist", &q)?["artist"]),
            "p" => items::playlist(&s, &self.get(&s, "getPlaylist", &q)?["playlist"]),
            _ => None,
        };
        it.ok_or_else(|| rpc_err(-32002, "not a music item"))
    }

    fn favorite(&self, p: &Value) -> Reply {
        let s = self.session()?;
        let (kind, id) = items::split_ref(p["ref"].as_str().unwrap_or(""))
            .ok_or_else(|| rpc_err(-32002, "no such item"))?;
        let key = match kind {
            "t" => "id",
            "a" => "albumId",
            "r" => "artistId",
            _ => return Err(rpc_err(-32003, "playlists cannot be starred")),
        };
        let endpoint = if p["on"].as_bool().unwrap_or(false) {
            "star"
        } else {
            "unstar"
        };
        self.get(&s, endpoint, &[(key, id.to_string())])
            .map(|_| Value::Null)
    }

    // -------------------------------------------------------------- library

    fn library(&self, method: &str, p: &Value) -> Reply {
        let s = self.session()?;
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(PAGE).clamp(1, PAGE);
        match method {
            "library.albums" => self.album_list(&s, "alphabeticalByName", offset, limit),
            "library.artists" => Ok(page(self.all_artists(&s)?, offset, limit)),
            _ => {
                // OpenSubsonic: an empty search3 query returns everything.
                let v = self.get(
                    &s,
                    "search3",
                    &[
                        ("query", String::new()),
                        ("artistCount", "0".into()),
                        ("albumCount", "0".into()),
                        ("songCount", limit.to_string()),
                        ("songOffset", offset.to_string()),
                    ],
                )?;
                let list = items::many(&s, &v["searchResult3"], "song", items::song);
                let has_more = list.len() as u64 == limit;
                Ok(json!({"items": list, "has_more": has_more}))
            }
        }
    }

    // -------------------------------------------------------------- resolve

    fn resolve(&self, p: &Value) -> Reply {
        let s = self.session()?;
        let r = p["ref"].as_str().unwrap_or("");
        let Some(("t", id)) = items::split_ref(r) else {
            return Err(rpc_err(-32002, "not a track"));
        };
        let v = self.get(&s, "getSong", &[("id", id.to_string())])?;
        let song = &v["song"];
        let format = items::format(song).unwrap_or(json!({}));
        let rate = format["sample_rate"].as_u64().map(|r| r as u32);
        let bits = format["bits"].as_u64().map(|b| b as u8);
        let plan = items::plan(&self.output.lock().unwrap(), rate, bits);
        let (url, format) = match plan {
            Plan::Direct => (s.url("stream", &[("id", id), ("format", "raw")]), format),
            Plan::Flac {
                rate: to,
                bits: depth,
            } => {
                let t = self.transcode(&s, id, to, depth).map_err(|why| {
                    let what = match bits {
                        Some(b) => format!("{} Hz / {b} bits", rate.unwrap_or(0)),
                        None => format!("{} Hz", rate.unwrap_or(0)),
                    };
                    eprintln!("{r}: {what} does not fit this output, no transcode: {why}");
                    rpc_err(-32003, format!("the DAC cannot take {what}: {why}"))
                })?;
                eprintln!("{r}: {to} Hz / {depth} bits transcode for this output");
                t
            }
        };
        let mut res = json!({
            "url": url,
            "duration_ms": song["duration"].as_i64().map(|d| d * 1000),
            "format": format,
            "live": false,
        });
        // OpenSubsonic `replayGain`, from the file's tags.
        let rg = &song["replayGain"];
        if let Some(g) = rg["trackGain"].as_f64() {
            res["replaygain"] = json!({ "track_gain": g });
            if let Some(pk) = rg["trackPeak"].as_f64().filter(|p| *p > 0.0) {
                res["replaygain"]["track_peak"] = pk.into();
            }
        }
        if let Some(o) = res.as_object_mut() {
            o.retain(|_, v| !v.is_null());
        }
        Ok(res)
    }

    /// A FLAC stream at `rate` / `bits` through the OpenSubsonic
    /// `transcoding` extension (Navidrome 0.64 and later): the stream URL and
    /// the format it delivers. The reason as text when the server cannot.
    fn transcode(
        &self,
        s: &Session,
        id: &str,
        rate: u32,
        bits: u8,
    ) -> Result<(String, Value), String> {
        let limit = |name: &str, v: u32| json!({"name": name, "comparison": "LessThanEqual", "values": [v.to_string()], "required": true});
        let client = json!({
            "name": "ricercar",
            "platform": "linux",
            "directPlayProfiles": [],
            "transcodingProfiles": [{"container": "flac", "audioCodec": "flac", "protocol": "http"}],
            "codecProfiles": [{"type": "AudioCodec", "name": "flac", "limitations": [
                limit("audioSamplerate", rate), limit("audioBitdepth", bits.into()),
            ]}],
        });
        let q = [
            ("mediaId", id.to_string()),
            ("mediaType", "song".to_string()),
        ];
        let d = match self.client.post(s, "getTranscodeDecision", &q, &client) {
            Ok(v) => v["transcodeDecision"].clone(),
            Err(e @ (Error::Auth | Error::Network(_))) => return Err(self.fail(e).message),
            Err(_) => {
                return Err("the server cannot transcode (no OpenSubsonic transcoding)".into());
            }
        };
        let params = d["transcodeParams"]
            .as_str()
            .filter(|_| d["canTranscode"] == true);
        let Some(params) = params else {
            let why = d["errorReason"]
                .as_str()
                .unwrap_or("the server declined to transcode");
            return Err(why.to_string());
        };
        let t = &d["transcodeStream"];
        let got = t["audioSamplerate"].as_u64().unwrap_or(rate.into()) as u32;
        let depth = t["audioBitdepth"].as_u64().unwrap_or(bits.into()) as u8;
        if items::plan(&self.output.lock().unwrap(), Some(got), Some(depth)) != Plan::Direct {
            return Err(format!("the server offers {got} Hz / {depth} bits only"));
        }
        let mut format = json!({"sample_rate": got, "bits": depth, "codec": "flac"});
        if let Some(c) = t["audioChannels"].as_u64() {
            format["channels"] = c.into();
        }
        let url = s.url(
            "getTranscodeStream",
            &[
                ("mediaId", id),
                ("mediaType", "song"),
                ("offset", "0"),
                ("transcodeParams", params),
            ],
        );
        Ok((url, format))
    }

    // ------------------------------------------------------------ reporting

    /// `scrobble` with `submission=false` while a track plays (the server's
    /// "now playing"), and `submission=true` once it counts as played: to
    /// its end, or half of it, or four minutes.
    fn report(&self, method: &str, p: &Value) {
        let Ok(s) = self.session() else {
            return;
        };
        let r = p["ref"].as_str().unwrap_or("");
        let Some(("t", id)) = items::split_ref(r) else {
            return;
        };
        let mut q = vec![("id", id.to_string())];
        match method {
            "playback.started" => {
                self.started.lock().unwrap().insert(r.to_string(), now_ms());
                q.push(("submission", "false".into()));
            }
            "playback.ended" => {
                let start = self.started.lock().unwrap().remove(r);
                let listened = p["listened_ms"].as_i64().unwrap_or(0);
                let played = p["reason"] == "ended" || listened >= 240_000 || {
                    let dur = self
                        .client
                        .get(&s, "getSong", &[("id", id.to_string())])
                        .ok()
                        .and_then(|v| v["song"]["duration"].as_i64())
                        .unwrap_or(0);
                    dur > 0 && listened * 2 >= dur * 1000
                };
                if !played {
                    return;
                }
                let time = start.unwrap_or_else(|| now_ms().saturating_sub(listened as u64));
                q.push(("submission", "true".into()));
                q.push(("time", time.to_string()));
            }
            _ => return,
        }
        if let Err(e) = self.client.get(&s, "scrobble", &q) {
            eprintln!("{method}: {e}");
        }
    }

    // ------------------------------------------------------------- dispatch

    fn handle(self: &Arc<Self>, method: &str, p: &Value) -> Reply {
        match method {
            "initialize" => self.initialize(p),
            "auth.status" => Ok(self.auth_status()),
            "auth.begin" => self.auth_begin(),
            "auth.complete" => self.auth_complete(p),
            "auth.sign_out" => self.sign_out(),
            "browse.root" => self.root(),
            "browse.list" => self.list(p),
            "search" => self.search(p),
            "item.get" => self.item_get(p),
            "favorites.set" => self.favorite(p),
            "library.albums" | "library.artists" | "library.tracks" => self.library(method, p),
            "track.resolve" => self.resolve(p),
            _ => Err(rpc_err(-32601, format!("method not found: {method}"))),
        }
    }
}

fn page(all: Vec<Value>, offset: u64, limit: u64) -> Value {
    let total = all.len() as u64;
    let items: Vec<Value> = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    json!({"items": items, "total": total, "has_more": offset + limit < total})
}

fn main() {
    let mut server_hint = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--server" => server_hint = args.next().unwrap_or_default(),
            "--version" => {
                println!("ricercar-subsonic {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => eprintln!("unknown option {a}"),
        }
    }
    let out = Arc::new(Out(Mutex::new(std::io::stdout())));
    let plugin = Arc::new(Plugin {
        out: out.clone(),
        server_hint,
        data_dir: Mutex::new(std::env::temp_dir()),
        french: Mutex::new(false),
        output: Mutex::new(Output::default()),
        client: Arc::new(Client::new()),
        session: Mutex::new(None),
        expired: Mutex::new(false),
        login: Mutex::new(None),
        artists: Mutex::new(None),
        started: Mutex::new(HashMap::new()),
    });

    for line in BufReader::new(std::io::stdin()).lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = msg["method"].as_str().map(str::to_string) else {
            continue; // an answer; this plugin sends no requests
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = msg.get("id").cloned() else {
            // Notifications.
            match method.as_str() {
                "output.changed" => {
                    *plugin.output.lock().unwrap() = Output::from_json(&params["output"]);
                }
                m if m.starts_with("playback.") => {
                    let plugin = plugin.clone();
                    std::thread::spawn(move || plugin.report(&method, &params));
                }
                _ => {}
            }
            continue;
        };
        if method == "shutdown" {
            out.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
            return;
        }
        let first = method == "initialize";
        let run = {
            let plugin = plugin.clone();
            let out = out.clone();
            move || {
                let reply = match plugin.handle(&method, &params) {
                    Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
                    Err(e) => json!({"jsonrpc": "2.0", "id": id,
                                     "error": {"code": e.code, "message": e.message}}),
                };
                out.send(reply);
            }
        };
        // The handshake first, in order; everything else may overlap.
        if first {
            run();
        } else {
            std::thread::spawn(run);
        }
    }
}
