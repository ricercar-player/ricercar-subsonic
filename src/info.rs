//! Lyrics and details: Subsonic answers → `lyrics.get` and `item.details`
//! answers, and the plain text the host wants from the HTML servers send.

use serde_json::{Value, json};

/// A list that may come as a lone object (see `items::many`).
fn list(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        o @ Value::Object(_) => vec![o],
        _ => Vec::new(),
    }
}

/// Whether a lyrics language tag (ISO 639, two or three letters, as tags
/// carry it) is the host's language (`fr`, `en`…).
fn same_language(tag: &str, lang: &str) -> bool {
    let tag = tag.to_lowercase();
    let tag = tag.split(['-', '_']).next().unwrap_or("");
    if lang.is_empty() || tag.is_empty() {
        return false;
    }
    // The three-letter codes of the most common languages.
    let three: &[&str] = match lang {
        "en" => &["eng"],
        "fr" => &["fra", "fre"],
        "de" => &["deu", "ger"],
        "es" => &["spa"],
        "it" => &["ita"],
        "pt" => &["por"],
        "nl" => &["nld", "dut"],
        "ja" => &["jpn"],
        "ko" => &["kor"],
        "zh" => &["zho", "chi"],
        "ru" => &["rus"],
        "sv" => &["swe"],
        "pl" => &["pol"],
        _ => &[],
    };
    tag == lang || three.contains(&tag)
}

/// OpenSubsonic `getLyricsBySongId` (`lyricsList.structuredLyrics`) as a
/// `lyrics.get` answer: synced lines when some are, else plain text. With
/// several lyrics, synced ones first, then those in the host's language
/// `lang`, then the server's order. `None` when there are none.
pub fn structured_lyrics(v: &Value, lang: &str) -> Option<Value> {
    let all = list(&v["lyricsList"]["structuredLyrics"]);
    let lines = |l: &Value| -> Vec<(Option<i64>, String)> {
        list(&l["line"])
            .into_iter()
            .map(|x| {
                let text = x["value"].as_str().unwrap_or("").trim_end().to_string();
                (x["start"].as_i64(), text)
            })
            .collect()
    };
    let usable: Vec<&Value> = all
        .into_iter()
        .filter(|l| lines(l).iter().any(|(_, t)| !t.trim().is_empty()))
        .collect();
    let rank = |l: &Value| {
        let lang_ok = l["lang"].as_str().is_some_and(|t| same_language(t, lang));
        (l["synced"] == true, lang_ok)
    };
    // The first of the best: `max_by_key` keeps the last of equals.
    let best = usable.iter().rev().max_by_key(|l| rank(l))?;
    let lines = lines(best);
    if best["synced"] == true {
        // `offset`, in ms: positive shows the lines sooner.
        let offset = best["offset"].as_i64().unwrap_or(0);
        let synced: Vec<Value> = lines
            .into_iter()
            .filter_map(|(start, text)| {
                let t = (start? - offset).max(0);
                Some(json!({"time_ms": t, "text": text}))
            })
            .collect();
        if !synced.is_empty() {
            return Some(json!({ "synced": synced }));
        }
        return None;
    }
    let plain: Vec<String> = lines.into_iter().map(|(_, t)| t).collect();
    Some(json!({ "plain": plain.join("\n").trim().to_string() }))
}

/// `getLyrics` (`lyrics.value`, plain text) as a `lyrics.get` answer.
pub fn plain_lyrics(v: &Value) -> Option<Value> {
    let text = v["lyrics"]["value"].as_str()?.replace("\r\n", "\n");
    let text = text.trim();
    (!text.is_empty()).then(|| json!({ "plain": text }))
}

/// Plain text from the HTML of a biography or album notes: tags dropped,
/// line breaks kept for `<br>` and paragraphs, entities decoded, spaces
/// collapsed. The "Read more on Last.fm" link that closes Last.fm texts
/// goes too (the source is named apart).
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(['<', '&']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with('<') {
            let end = rest.find('>').map_or(rest.len(), |e| e + 1);
            let tag = rest[1..]
                .split('>')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            match name {
                "br" => out.push('\n'),
                "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "tr" => out.push_str("\n\n"),
                // `<a …>Read more on Last.fm</a>`: the link and its text.
                "a" if !tag.starts_with('/') && tag.contains("last.fm") => {
                    let close = rest.to_ascii_lowercase().find("</a>");
                    let body = close.map(|c| &rest[end.min(c)..c]).unwrap_or("");
                    if body.to_lowercase().contains("read more") {
                        rest = &rest[close.map_or(rest.len(), |c| c + 4)..];
                        continue;
                    }
                }
                _ => {}
            }
            rest = &rest[end..];
        } else {
            let end = rest[1..]
                .find(|c: char| c == ';' || c == '&' || c == '<' || c.is_whitespace())
                .map(|e| e + 1);
            match end.filter(|e| rest[*e..].starts_with(';')) {
                Some(e) => {
                    match entity(&rest[1..e]) {
                        Some(c) => out.push(c),
                        None => out.push_str(&rest[..=e]),
                    }
                    rest = &rest[e + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
    }
    out.push_str(rest);
    // Spaces collapsed within lines; at most one empty line in a row.
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        text.push_str(&line);
    }
    text
}

fn entity(name: &str) -> Option<char> {
    if let Some(n) = name.strip_prefix('#') {
        let code = match n.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => n.parse().ok()?,
        };
        return char::from_u32(code).filter(|c| !c.is_control() || *c == '\n');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "rsquo" => '’',
        "lsquo" => '‘',
        "rdquo" => '”',
        "ldquo" => '“',
        "laquo" => '«',
        "raquo" => '»',
        "eacute" => 'é',
        "egrave" => 'è',
        "copy" => '©',
        _ => return None,
    })
}

/// A biography or album notes as `{text, source?}`, when there is text.
/// The source is named when the HTML shows it.
pub fn biography(html: &str) -> Option<Value> {
    let text = strip_html(html);
    if text.is_empty() {
        return None;
    }
    let lower = html.to_lowercase();
    let source = if lower.contains("last.fm") {
        Some("Last.fm")
    } else if lower.contains("wikipedia.org") {
        Some("Wikipedia")
    } else {
        None
    };
    let mut b = json!({ "text": text });
    if let Some(s) = source {
        b["source"] = s.into();
    }
    Some(b)
}

/// `2021-03-05` from OpenSubsonic's `{year, month, day}`, as far as known.
fn date(v: &Value) -> Option<String> {
    let y = v["year"].as_i64().filter(|y| *y > 0)?;
    Some(match (v["month"].as_i64(), v["day"].as_i64()) {
        (Some(m), Some(d)) if m > 0 && d > 0 => format!("{y}-{m:02}-{d:02}"),
        (Some(m), _) if m > 0 => format!("{y}-{m:02}"),
        _ => y.to_string(),
    })
}

/// `1 h 02 min`, `42 min`.
fn duration(secs: i64) -> String {
    let m = (secs + 30) / 60;
    if m >= 60 {
        format!("{} h {:02} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

/// Facts about an album (`AlbumID3`, with OpenSubsonic's fields when the
/// server has them): label, genres, release type, dates, length.
pub fn album_facts(v: &Value, fr: bool) -> Vec<Value> {
    let t = |en: &'static str, f: &'static str| if fr { f } else { en };
    let names = |key: &str, field: Option<&str>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for x in list(&v[key]) {
            let n = match field {
                Some(f) => x[f].as_str(),
                None => x.as_str(),
            };
            if let Some(n) = n.map(str::trim).filter(|n| !n.is_empty())
                && !out.iter().any(|o| o.eq_ignore_ascii_case(n))
            {
                out.push(n.to_string());
            }
        }
        out
    };
    let mut facts = Vec::new();
    let mut fact = |label: &str, value: String| {
        if !value.is_empty() {
            facts.push(json!({"label": label, "value": value}));
        }
    };
    fact(
        t("Label", "Label"),
        names("recordLabels", Some("name")).join(", "),
    );
    let mut genres = names("genres", Some("name"));
    if genres.is_empty()
        && let Some(g) = v["genre"].as_str().map(str::trim).filter(|g| !g.is_empty())
    {
        genres.push(g.to_string());
    }
    fact(t("Genre", "Genre"), genres.join(", "));
    fact(
        t("Release type", "Type de parution"),
        names("releaseTypes", None).join(", "),
    );
    let released = date(&v["releaseDate"])
        .or_else(|| v["year"].as_i64().filter(|y| *y > 0).map(|y| y.to_string()));
    let original = date(&v["originalReleaseDate"]);
    fact(
        t("Released", "Parution"),
        released.clone().unwrap_or_default(),
    );
    if original.is_some() && original != released {
        fact(
            t("Original release", "Première parution"),
            original.unwrap_or_default(),
        );
    }
    if let Some(n) = v["songCount"].as_i64().filter(|n| *n > 0) {
        fact(t("Tracks", "Pistes"), n.to_string());
    }
    if let Some(d) = v["duration"].as_i64().filter(|d| *d > 0) {
        fact(t("Length", "Durée"), duration(d));
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text() {
        assert_eq!(
            strip_html(
                "Ensemble is a <b>jazz</b> group &amp; more.<br/>Formed in 2001 &mdash; &#233;t&#xE9;.\n\n\n<p>Second  paragraph</p> <a href=\"https://www.last.fm/music/Ensemble\">Read more on Last.fm</a>"
            ),
            "Ensemble is a jazz group & more.\nFormed in 2001 — été.\n\nSecond paragraph"
        );
        assert_eq!(
            strip_html("a &unknown; b & c &lt;x&gt;"),
            "a &unknown; b & c <x>"
        );
        assert_eq!(strip_html("<a href=\"x\">link</a> text <"), "link text");
        assert_eq!(strip_html("  "), "");
        // Byte positions stay on characters.
        assert_eq!(
            strip_html("İ <a href=\"last.fm\">İ read more</a> x <é"),
            "İ x"
        );
        // Links elsewhere keep their text.
        assert_eq!(
            strip_html("See <a href=\"https://www.last.fm/x\">the page</a>."),
            "See the page."
        );
    }

    #[test]
    fn biographies() {
        let b = biography(
            "A duo. <a target='_blank' href=\"https://www.last.fm/music/Duo\" rel=\"nofollow\">Read more on Last.fm</a>",
        )
        .unwrap();
        assert_eq!(b, json!({"text": "A duo.", "source": "Last.fm"}));
        assert_eq!(
            biography("Plain notes."),
            Some(json!({"text": "Plain notes."}))
        );
        assert_eq!(biography("<p> </p>"), None);
    }

    #[test]
    fn lyrics_synced_first() {
        let v = json!({"lyricsList": {"structuredLyrics": [
            {"lang": "eng", "synced": false, "line": [{"value": "One"}, {"value": "Two"}]},
            {"lang": "xxx", "synced": true, "offset": 100,
             "line": [{"start": 50, "value": "One"}, {"start": 2300, "value": "Two "}]},
        ]}});
        assert_eq!(
            structured_lyrics(&v, "fr").unwrap(),
            json!({"synced": [{"time_ms": 0, "text": "One"}, {"time_ms": 2200, "text": "Two"}]})
        );
        let unsynced = json!({"lyricsList": {"structuredLyrics": {
            "synced": false, "line": [{"value": "One"}, {"value": ""}, {"value": "Two"}]
        }}});
        assert_eq!(
            structured_lyrics(&unsynced, "en").unwrap(),
            json!({"plain": "One\n\nTwo"})
        );
        assert_eq!(
            structured_lyrics(&json!({"lyricsList": {"structuredLyrics": []}}), "en"),
            None
        );
        assert_eq!(structured_lyrics(&json!({"lyricsList": {}}), "en"), None);
    }

    #[test]
    fn lyrics_language() {
        let v = json!({"lyricsList": {"structuredLyrics": [
            {"lang": "eng", "synced": true, "line": [{"start": 0, "value": "Hello"}]},
            {"lang": "fra", "synced": true, "line": [{"start": 0, "value": "Bonjour"}]},
        ]}});
        let text = |lang: &str| structured_lyrics(&v, lang).unwrap()["synced"][0]["text"].clone();
        assert_eq!(text("fr"), "Bonjour");
        assert_eq!(text("en"), "Hello");
        // Neither: the server's order.
        assert_eq!(text("de"), "Hello");
    }

    #[test]
    fn plain_lyrics_answer() {
        assert_eq!(
            plain_lyrics(&json!({"lyrics": {"artist": "A", "value": "La\r\nla\r\n"}})),
            Some(json!({"plain": "La\nla"}))
        );
        assert_eq!(plain_lyrics(&json!({"lyrics": {"value": " "}})), None);
        assert_eq!(plain_lyrics(&json!({"lyrics": {}})), None);
    }

    #[test]
    fn facts() {
        let v = json!({
            "id": "a1", "name": "Sessions", "year": 2021, "songCount": 9, "duration": 3725,
            "genre": "Jazz", "genres": [{"name": "Jazz"}, {"name": "jazz"}, {"name": "Soul"}],
            "recordLabels": [{"name": "Blue Room"}],
            "releaseTypes": ["Album", "Live"],
            "releaseDate": {"year": 2021, "month": 3, "day": 5},
            "originalReleaseDate": {"year": 1999}
        });
        let f = album_facts(&v, false);
        let pairs: Vec<(String, String)> = f
            .iter()
            .map(|x| {
                (
                    x["label"].as_str().unwrap().to_string(),
                    x["value"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let want = [
            ("Label", "Blue Room"),
            ("Genre", "Jazz, Soul"),
            ("Release type", "Album, Live"),
            ("Released", "2021-03-05"),
            ("Original release", "1999"),
            ("Tracks", "9"),
            ("Length", "1 h 02 min"),
        ];
        assert_eq!(
            pairs,
            want.map(|(a, b)| (a.to_string(), b.to_string())).to_vec()
        );
        // An older server: `genre` and `year` only.
        let f = album_facts(&json!({"genre": "Rock", "year": 1977}), true);
        assert_eq!(
            f,
            vec![
                json!({"label": "Genre", "value": "Rock"}),
                json!({"label": "Parution", "value": "1977"})
            ]
        );
    }
}
