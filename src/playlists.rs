//! Editing the user's own playlists (`playlist_edit`) with
//! `createPlaylist`, `updatePlaylist` and `deletePlaylist`.
//!
//! Subsonic removes entries by position, so an `entry_id` carries the
//! position and the song id (see `items::entry_id`): an edit whose entries
//! no longer match the playlist is refused rather than removing the wrong
//! songs.

use serde_json::Value;

use crate::subsonic::Session;
use crate::{Plugin, Reply, RpcError, items, rpc_err};

/// Songs added, or positions removed, per `updatePlaylist` request: query
/// strings stay well under the 8 KiB some servers cap a request line at.
const BATCH: usize = 50;
/// Longest playlist reordered with a query string, on servers without the
/// OpenSubsonic `formPost` extension.
const MOVE_MAX_QUERY: usize = 100;

/// The song ids of a `getPlaylist` answer, in order.
fn song_ids(pl: &Value) -> Vec<String> {
    let list = match &pl["entry"] {
        Value::Array(a) => a.iter().collect(),
        o @ Value::Object(_) => vec![o],
        _ => Vec::new(),
    };
    list.iter()
        .map(|e| e["id"].as_str().unwrap_or("").to_string())
        .collect()
}

/// The positions `entries` point at in a playlist of `ids`, highest first
/// and each once. An error when one no longer matches (the playlist
/// changed since it was listed) or is not an entry id at all.
fn positions(ids: &[String], entries: &[&str]) -> Result<Vec<usize>, RpcError> {
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let (i, song) =
            items::entry(e).ok_or_else(|| rpc_err(-32602, "not an entry of this playlist"))?;
        if ids.get(i).map(String::as_str) != Some(song) {
            return Err(rpc_err(
                -32602,
                "the playlist changed since it was listed: reload it",
            ));
        }
        out.push(i);
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out.dedup();
    Ok(out)
}

/// `ids` with the entry at `from` moved to `to` (counted once it is taken
/// out, and kept within the playlist).
fn moved(ids: &[String], from: usize, to: usize) -> Vec<String> {
    let mut out = ids.to_vec();
    let e = out.remove(from);
    let to = to.min(out.len());
    out.insert(to, e);
    out
}

impl Plugin {
    fn playlist_id<'a>(&self, p: &'a Value) -> Result<&'a str, RpcError> {
        match items::split_ref(p["ref"].as_str().unwrap_or("")) {
            Some(("p", id)) => Ok(id),
            _ => Err(rpc_err(-32002, "not a playlist")),
        }
    }

    /// `getPlaylist`, when the signed-in user may edit it.
    fn own_playlist(&self, s: &Session, id: &str) -> Result<Value, RpcError> {
        let v = self.get(s, "getPlaylist", &[("id", id.to_string())])?;
        let pl = v["playlist"].clone();
        if !items::editable(s, &pl) {
            return Err(rpc_err(-32602, "this playlist cannot be edited"));
        }
        Ok(pl)
    }

    /// The songs of an editable playlist, when the server lists them all:
    /// positions only mean something then.
    fn own_songs(&self, s: &Session, id: &str) -> Result<Vec<String>, RpcError> {
        let pl = self.own_playlist(s, id)?;
        let ids = song_ids(&pl);
        if pl["songCount"]
            .as_u64()
            .is_some_and(|n| n as usize != ids.len())
        {
            return Err(rpc_err(
                -32003,
                "the server lists only part of this playlist",
            ));
        }
        Ok(ids)
    }

    pub(crate) fn playlist_edit(&self, method: &str, p: &Value) -> Reply {
        let s = self.session()?;
        match method {
            "playlists.create" => self.playlist_create(&s, p),
            "playlists.rename" => {
                let id = self.playlist_id(p)?;
                let name = name(p)?;
                self.own_playlist(&s, id)?;
                self.get(
                    &s,
                    "updatePlaylist",
                    &[("playlistId", id.to_string()), ("name", name)],
                )?;
                Ok(Value::Null)
            }
            "playlists.delete" => {
                let id = self.playlist_id(p)?;
                self.own_playlist(&s, id)?;
                self.get(&s, "deletePlaylist", &[("id", id.to_string())])?;
                Ok(Value::Null)
            }
            "playlists.add" => self.playlist_add(&s, p),
            "playlists.remove" => self.playlist_remove(&s, p),
            "playlists.move" => self.playlist_move(&s, p),
            _ => Err(rpc_err(-32601, format!("method not found: {method}"))),
        }
    }

    /// `createPlaylist`, then `updatePlaylist` for the description
    /// (`comment`) and visibility. Servers before API 1.14 answer with no
    /// playlist: it is then the newest of that name.
    fn playlist_create(&self, s: &Session, p: &Value) -> Reply {
        let name = name(p)?;
        let v = self.get(s, "createPlaylist", &[("name", name.clone())])?;
        let id = match v["playlist"]["id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let v = self.get(s, "getPlaylists", &[])?;
                let list = match &v["playlists"]["playlist"] {
                    Value::Array(a) => a.clone(),
                    o @ Value::Object(_) => vec![o.clone()],
                    _ => Vec::new(),
                };
                list.iter()
                    .filter(|x| x["name"].as_str() == Some(&name) && items::editable(s, x))
                    .max_by_key(|x| x["created"].as_str().unwrap_or("").to_string())
                    .and_then(|x| x["id"].as_str().map(str::to_string))
                    .ok_or_else(|| rpc_err(-32603, "the server did not return the new playlist"))?
            }
        };
        let mut q = vec![("playlistId", id.clone())];
        if let Some(d) = p["description"]
            .as_str()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            q.push(("comment", d.to_string()));
        }
        if let Some(public) = p["public"].as_bool() {
            q.push(("public", public.to_string()));
        }
        if q.len() > 1 {
            self.get(s, "updatePlaylist", &q)?;
        }
        let v = self.get(s, "getPlaylist", &[("id", id)])?;
        items::playlist(s, &v["playlist"])
            .ok_or_else(|| rpc_err(-32603, "the server did not return the new playlist"))
    }

    /// `updatePlaylist?songIdToAdd=…`, in order, a batch at a time.
    fn playlist_add(&self, s: &Session, p: &Value) -> Reply {
        let id = self.playlist_id(p)?;
        let mut songs = Vec::new();
        for r in p["items"].as_array().into_iter().flatten() {
            match items::split_ref(r.as_str().unwrap_or("")) {
                Some(("t", song)) => songs.push(song.to_string()),
                _ => return Err(rpc_err(-32602, "only tracks go in a playlist")),
            }
        }
        if songs.is_empty() {
            return Err(rpc_err(-32602, "no tracks to add"));
        }
        self.own_playlist(s, id)?;
        for batch in songs.chunks(BATCH) {
            let mut q = vec![("playlistId", id.to_string())];
            q.extend(batch.iter().map(|x| ("songIdToAdd", x.clone())));
            self.get(s, "updatePlaylist", &q)?;
        }
        Ok(Value::Null)
    }

    /// `updatePlaylist?songIndexToRemove=…`, highest positions first so
    /// that each batch leaves the next ones in place.
    fn playlist_remove(&self, s: &Session, p: &Value) -> Reply {
        let id = self.playlist_id(p)?;
        let entries: Vec<&str> = p["entries"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if entries.is_empty() {
            return Err(rpc_err(-32602, "no entries to remove"));
        }
        let ids = self.own_songs(s, id)?;
        let at = positions(&ids, &entries)?;
        for batch in at.chunks(BATCH) {
            let mut q = vec![("playlistId", id.to_string())];
            q.extend(batch.iter().map(|i| ("songIndexToRemove", i.to_string())));
            self.get(s, "updatePlaylist", &q)?;
        }
        Ok(Value::Null)
    }

    /// The Subsonic API cannot move an entry; `createPlaylist` with a
    /// `playlistId` replaces the whole song list instead. Only done on
    /// servers known to replace (not append) that way, and checked
    /// afterwards. Others answer "method not found", as the protocol asks.
    fn playlist_move(&self, s: &Session, p: &Value) -> Reply {
        let server = s.server_name.to_lowercase();
        if !(server.starts_with("navidrome") || server.starts_with("gonic")) {
            return Err(rpc_err(
                -32601,
                "method not found: playlists.move (not on this server)",
            ));
        }
        let id = self.playlist_id(p)?;
        let entry = p["entry"].as_str().unwrap_or("");
        let to = p["to"]
            .as_u64()
            .ok_or_else(|| rpc_err(-32602, "a target position is needed"))?
            as usize;
        let ids = self.own_songs(s, id)?;
        let from = positions(&ids, &[entry])?[0];
        let order = moved(&ids, from, to);
        if order == ids {
            return Ok(Value::Null);
        }
        let mut q = vec![("playlistId", id.to_string())];
        q.extend(order.iter().map(|x| ("songId", x.clone())));
        if self.has_extension(s, "formPost") {
            self.client
                .post_form(s, "createPlaylist", &q)
                .map_err(|e| self.fail(e))?;
        } else if order.len() <= MOVE_MAX_QUERY {
            self.get(s, "createPlaylist", &q)?;
        } else {
            return Err(rpc_err(
                -32003,
                "this server cannot reorder a playlist that long",
            ));
        }
        let now = self.get(s, "getPlaylist", &[("id", id.to_string())])?;
        if song_ids(&now["playlist"]) != order {
            eprintln!("playlists.move: the server did not keep the new order");
            return Err(rpc_err(-32603, "the server did not keep the new order"));
        }
        Ok(Value::Null)
    }
}

/// A playlist name: trimmed, one line, not empty.
fn name(p: &Value) -> Result<String, RpcError> {
    let n: String = p["name"]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let n = n.trim();
    if n.is_empty() {
        return Err(rpc_err(-32602, "a playlist needs a name"));
    }
    Ok(n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn entry_ids() {
        assert_eq!(items::entry_id(3, "s-1"), "3:s-1");
        assert_eq!(items::entry("3:s-1"), Some((3, "s-1")));
        // Ids may hold colons themselves.
        assert_eq!(items::entry("0:a:b"), Some((0, "a:b")));
        assert_eq!(items::entry(":s"), None);
        assert_eq!(items::entry("x:s"), None);
        assert_eq!(items::entry("2:"), None);
        assert_eq!(items::entry("s1"), None);
    }

    #[test]
    fn positions_checked() {
        let list = ids(&["a", "b", "a", "c"]);
        // Highest first, each once.
        assert_eq!(
            positions(&list, &["0:a", "2:a", "3:c", "2:a"]).unwrap(),
            [3, 2, 0]
        );
        // Stale: position 1 is no longer `c`, or is past the end.
        assert_eq!(positions(&list, &["1:c"]).unwrap_err().code, -32602);
        assert_eq!(positions(&list, &["9:a"]).unwrap_err().code, -32602);
        assert_eq!(positions(&list, &["b"]).unwrap_err().code, -32602);
    }

    #[test]
    fn moves() {
        let list = ids(&["a", "b", "c", "d"]);
        assert_eq!(moved(&list, 0, 2), ids(&["b", "c", "a", "d"]));
        assert_eq!(moved(&list, 3, 0), ids(&["d", "a", "b", "c"]));
        assert_eq!(moved(&list, 1, 99), ids(&["a", "c", "d", "b"]));
        assert_eq!(moved(&list, 2, 2), list);
    }

    #[test]
    fn songs_of_a_playlist() {
        assert_eq!(
            song_ids(&json!({"entry": [{"id": "1"}, {"id": "2"}]})),
            ids(&["1", "2"])
        );
        assert_eq!(song_ids(&json!({"entry": {"id": "1"}})), ids(&["1"]));
        assert!(song_ids(&json!({})).is_empty());
    }

    #[test]
    fn names() {
        assert_eq!(
            name(&json!({"name": " Evening\nmix "})).unwrap(),
            "Eveningmix"
        );
        assert_eq!(name(&json!({"name": "  "})).unwrap_err().code, -32602);
        assert_eq!(name(&json!({})).unwrap_err().code, -32602);
    }
}
