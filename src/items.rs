//! Subsonic entries → ricercar items, and whether the DAC takes a file.
//!
//! Refs are `<prefix>/<subsonic id>`: `t` track (song), `a` album, `r`
//! artist, `p` playlist. Top-level sections use bare words (`albums`,
//! `artists`…).

use serde_json::{Value, json};

use crate::subsonic::Session;

/// Kind prefix and id of a ref, for refs that point at a Subsonic entry.
/// Ids are opaque strings whose shape differs between servers
/// (`al-12`, `4f1c…`, `1234`).
pub fn split_ref(r: &str) -> Option<(&str, &str)> {
    let (k, id) = r.split_once('/')?;
    let ok = matches!(k, "t" | "a" | "r" | "p")
        && !id.is_empty()
        && id.len() <= 512
        && !id.chars().any(char::is_control);
    ok.then_some((k, id))
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn num(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64).filter(|n| *n > 0)
}

fn join(parts: &[Option<String>]) -> Option<String> {
    let v: Vec<&str> = parts.iter().flatten().map(String::as_str).collect();
    (!v.is_empty()).then(|| v.join(" · "))
}

/// `{sample_rate, bits, channels, codec}` of the file as stored. The rate,
/// depth and channels are OpenSubsonic fields; older servers only give the
/// file suffix.
pub fn format(v: &Value) -> Option<Value> {
    let mut f = serde_json::Map::new();
    if let Some(r) = num(v, "samplingRate") {
        f.insert("sample_rate".into(), r.into());
    }
    if let Some(b) = num(v, "bitDepth") {
        f.insert("bits".into(), b.into());
    }
    if let Some(c) = num(v, "channelCount") {
        f.insert("channels".into(), c.into());
    }
    if let Some(c) = text(v, "suffix") {
        f.insert("codec".into(), c.to_lowercase().into());
    }
    (!f.is_empty()).then_some(Value::Object(f))
}

/// "A, B" from OpenSubsonic's `artists` / `albumArtists` lists, else the
/// display string, else the plain field.
fn names(v: &Value, list: &str, display: &str, plain: Option<&str>) -> Option<String> {
    let l: Vec<&str> = v[list]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    if l.is_empty() {
        text(v, display).or_else(|| plain.and_then(|k| text(v, k)))
    } else {
        Some(l.join(", "))
    }
}

fn genre(v: &Value) -> Option<String> {
    v["genres"][0]["name"]
        .as_str()
        .map(str::to_string)
        .or_else(|| text(v, "genre"))
}

fn finish(s: &Session, v: &Value, mut it: Value) -> Value {
    if let Some(c) = text(v, "coverArt") {
        it["art"] = s.cover(&c).into();
    }
    // Leave optional fields out rather than send nulls.
    if let Some(o) = it.as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    it
}

/// A song (`Child`). Videos and directories are left out.
pub fn song(s: &Session, v: &Value) -> Option<Value> {
    let id = v["id"].as_str()?;
    if v["isDir"] == true || v["isVideo"] == true {
        return None;
    }
    let title = text(v, "title").unwrap_or_else(|| "?".into());
    let artist = names(v, "artists", "displayArtist", Some("artist"));
    let album = text(v, "album");
    let it = json!({
        "ref": format!("t/{id}"),
        "kind": "track",
        "title": title,
        "subtitle": join(&[artist.clone(), album.clone()]),
        "artist": artist,
        "album": album,
        "album_artist": names(v, "albumArtists", "displayAlbumArtist", None),
        "track_no": num(v, "track"),
        "disc_no": num(v, "discNumber"),
        "year": num(v, "year"),
        "genre": genre(v),
        "duration_ms": num(v, "duration").map(|d| d * 1000),
        "format": format(v),
        "playable": true,
    });
    Some(finish(s, v, it))
}

/// An album (`AlbumID3`).
pub fn album(s: &Session, v: &Value) -> Option<Value> {
    let id = v["id"].as_str()?;
    let title = text(v, "name").or_else(|| text(v, "title"));
    let artist = names(v, "artists", "displayArtist", Some("artist"));
    let year = num(v, "year");
    let it = json!({
        "ref": format!("a/{id}"),
        "kind": "album",
        "title": title.clone().unwrap_or_else(|| "?".into()),
        "subtitle": join(&[artist.clone(), year.map(|y| y.to_string())]),
        "artist": artist,
        "album": title,
        "year": year,
        "genre": genre(v),
        "browsable": true,
    });
    Some(finish(s, v, it))
}

/// An artist (`ArtistID3`).
pub fn artist(s: &Session, v: &Value) -> Option<Value> {
    let id = v["id"].as_str()?;
    let name = text(v, "name");
    let mut it = finish(
        s,
        v,
        json!({
            "ref": format!("r/{id}"),
            "kind": "artist",
            "title": name.clone().unwrap_or_else(|| "?".into()),
            "subtitle": num(v, "albumCount").map(|n| format!("{n} ◫")),
            "artist": name,
            "browsable": true,
        }),
    );
    if it.get("art").is_none()
        && let Some(u) = text(v, "artistImageUrl").filter(|u| u.starts_with("https://"))
    {
        it["art"] = u.into();
    }
    Some(it)
}

pub fn playlist(s: &Session, v: &Value) -> Option<Value> {
    let id = v["id"].as_str()?;
    let it = json!({
        "ref": format!("p/{id}"),
        "kind": "playlist",
        "title": text(v, "name").unwrap_or_else(|| "?".into()),
        "subtitle": v["songCount"].as_i64().map(|n| format!("{n} ♪")),
        "browsable": true,
    });
    Some(finish(s, v, it))
}

/// Map `v[key]` (an array, or a lone object as some servers send for one
/// entry) with `f`.
pub fn many(
    s: &Session,
    v: &Value,
    key: &str,
    f: fn(&Session, &Value) -> Option<Value>,
) -> Vec<Value> {
    match &v[key] {
        Value::Array(a) => a.iter().filter_map(|x| f(s, x)).collect(),
        o @ Value::Object(_) => f(s, o).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// What the DAC takes natively, from `initialize` / `output.changed`.
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub bit_perfect: bool,
    pub max_rate: Option<u32>,
    pub max_bits: Option<u8>,
    pub rates: Vec<u32>,
}

impl Output {
    pub fn from_json(v: &Value) -> Output {
        Output {
            bit_perfect: v["bit_perfect"].as_bool().unwrap_or(false),
            max_rate: v["max_rate"].as_u64().map(|r| r as u32),
            max_bits: v["max_bits"].as_u64().map(|b| b as u8),
            rates: v["rates"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.as_u64())
                        .map(|r| r as u32)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn takes_rate(&self, rate: u32) -> bool {
        if self.rates.is_empty() {
            self.max_rate.is_none_or(|m| rate <= m)
        } else {
            self.rates.contains(&rate)
        }
    }
}

/// How to fetch a track so that the engine plays it without converting it.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// The original file, byte for byte.
    Direct,
    /// A FLAC transcode at a rate and depth the DAC accepts.
    Flac { rate: u32, bits: u8 },
}

/// Keep the original unless the DAC cannot take its rate or depth; then
/// ask for FLAC at the closest rate the DAC takes, preferring the same
/// family (44.1 kHz or 48 kHz multiples) and never going up. Unknown rates
/// (older servers) keep the original: the engine has the last word.
pub fn plan(out: &Output, rate: Option<u32>, bits: Option<u8>) -> Plan {
    let Some(rate) = rate else {
        return Plan::Direct;
    };
    let bits_ok = |b: u8| out.max_bits.is_none_or(|m| b <= m);
    if !out.bit_perfect || (out.takes_rate(rate) && bits.is_none_or(bits_ok)) {
        return Plan::Direct;
    }
    let mut candidates: Vec<u32> = if out.rates.is_empty() {
        [
            44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        ]
        .into_iter()
        .filter(|r| out.takes_rate(*r))
        .collect()
    } else {
        out.rates.clone()
    };
    candidates.sort_unstable();
    let family = |r: u32| r % 11_025 == 0;
    let target = candidates
        .iter()
        .rev()
        .find(|r| **r <= rate && family(**r) == family(rate))
        .or_else(|| candidates.iter().rev().find(|r| **r <= rate))
        .or_else(|| candidates.first())
        .copied()
        .unwrap_or(rate);
    let depth = bits.unwrap_or(16).min(out.max_bits.unwrap_or(24)).max(16);
    Plan::Flac {
        rate: target,
        bits: depth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::Auth;

    fn session() -> Session {
        Session {
            server: "http://nd".into(),
            auth: Auth::ApiKey {
                user: "a".into(),
                key: "k".into(),
            },
            server_name: String::new(),
        }
    }

    fn dac(rates: &[u32], bits: u8) -> Output {
        Output {
            bit_perfect: true,
            max_rate: rates.iter().max().copied(),
            max_bits: Some(bits),
            rates: rates.to_vec(),
        }
    }

    #[test]
    fn refs() {
        assert_eq!(split_ref("t/abc123"), Some(("t", "abc123")));
        assert_eq!(split_ref("a/al-12"), Some(("a", "al-12")));
        assert_eq!(split_ref("x/abc"), None);
        assert_eq!(split_ref("t/"), None);
        assert_eq!(split_ref("t/a\nb"), None);
        assert_eq!(split_ref("albums"), None);
    }

    #[test]
    fn song_mapping() {
        let v = json!({
            "id": "s1", "title": "Coda", "album": "Sessions", "albumId": "a1",
            "artist": "Ensemble feat. Guest", "track": 3, "discNumber": 1, "year": 2021,
            "genre": "Jazz", "coverArt": "al-a1", "duration": 245, "suffix": "FLAC",
            "samplingRate": 96000, "bitDepth": 24, "channelCount": 2,
            "artists": [{"id": "r1", "name": "Ensemble"}, {"id": "r2", "name": "Guest"}],
            "albumArtists": [{"id": "r1", "name": "Ensemble"}],
            "isDir": false
        });
        let it = song(&session(), &v).unwrap();
        assert_eq!(it["ref"], "t/s1");
        assert_eq!(it["kind"], "track");
        assert_eq!(it["artist"], "Ensemble, Guest");
        assert_eq!(it["album_artist"], "Ensemble");
        assert_eq!(it["subtitle"], "Ensemble, Guest · Sessions");
        assert_eq!(it["duration_ms"], 245_000);
        assert_eq!(it["track_no"], 3);
        assert_eq!(it["genre"], "Jazz");
        assert_eq!(
            it["format"],
            json!({"sample_rate": 96000, "bits": 24, "channels": 2, "codec": "flac"})
        );
        assert_eq!(
            it["art"],
            "http://nd/rest/getCoverArt?apiKey=k&v=1.16.1&c=ricercar&id=al-a1&size=600"
        );
    }

    #[test]
    fn old_server_song() {
        // Subsonic 1.16 without OpenSubsonic fields; a lossy file.
        let v = json!({"id": "7", "title": "T", "artist": "A", "suffix": "mp3", "bitDepth": 0});
        let it = song(&session(), &v).unwrap();
        assert_eq!(it["artist"], "A");
        assert_eq!(it["format"], json!({"codec": "mp3"}));
        assert!(it.get("art").is_none());
        assert!(it.get("album_artist").is_none());
        assert!(song(&session(), &json!({"id": "d", "isDir": true})).is_none());
    }

    #[test]
    fn album_artist_playlist() {
        let s = session();
        let a = album(
            &s,
            &json!({"id": "a1", "name": "Sessions", "artist": "Ensemble", "year": 2021}),
        )
        .unwrap();
        assert_eq!(a["subtitle"], "Ensemble · 2021");
        assert_eq!(a["browsable"], true);
        let r = artist(
            &s,
            &json!({"id": "r1", "name": "Ensemble", "albumCount": 2,
                    "artistImageUrl": "https://img/x.jpg"}),
        )
        .unwrap();
        assert_eq!(r["ref"], "r/r1");
        assert_eq!(r["art"], "https://img/x.jpg");
        let p = playlist(&s, &json!({"id": "p1", "name": "Mix", "songCount": 12})).unwrap();
        assert_eq!(p["subtitle"], "12 ♪");
        // A lone object where an array was expected.
        assert_eq!(
            many(&s, &json!({"song": {"id": "1"}}), "song", song).len(),
            1
        );
        assert!(many(&s, &json!({}), "song", song).is_empty());
    }

    #[test]
    fn plans() {
        let usb = dac(&[44_100, 48_000, 88_200, 96_000], 24);
        assert_eq!(plan(&usb, Some(96_000), Some(24)), Plan::Direct);
        assert_eq!(plan(&usb, Some(44_100), None), Plan::Direct);
        assert_eq!(plan(&usb, None, None), Plan::Direct);
        assert_eq!(
            plan(&usb, Some(176_400), Some(24)),
            Plan::Flac {
                rate: 88_200,
                bits: 24
            }
        );
        assert_eq!(
            plan(&usb, Some(192_000), Some(32)),
            Plan::Flac {
                rate: 96_000,
                bits: 24
            }
        );
        let cd = dac(&[44_100], 16);
        assert_eq!(
            plan(&cd, Some(48_000), Some(24)),
            Plan::Flac {
                rate: 44_100,
                bits: 16
            }
        );
        // The null sink, a PipeWire default: the engine converts.
        assert_eq!(
            plan(&Output::default(), Some(384_000), Some(32)),
            Plan::Direct
        );
    }
}
