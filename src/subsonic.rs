//! A small client for the Subsonic API, as documented by OpenSubsonic
//! (https://opensubsonic.netlify.app): sign-in, catalogue queries, stars and
//! scrobbles. Works with any server of API version 1.13 or later (token
//! authentication), plus the OpenSubsonic API-key extension when offered.

use std::time::Duration;

use md5::{Digest, Md5};
use serde_json::{Value, json};

/// API version sent with every request: the last one of the original
/// Subsonic API, which OpenSubsonic servers all accept.
const API: &str = "1.16.1";
const CLIENT: &str = "ricercar";

/// How the plugin proves who it is, as stored in `<data_dir>/auth.json`.
#[derive(Clone, Debug, PartialEq)]
pub enum Auth {
    /// `u`, `t = md5(password + salt)`, `s`. The password itself is not kept.
    Token {
        user: String,
        salt: String,
        token: String,
    },
    /// OpenSubsonic `apiKeyAuthentication`: `apiKey` alone.
    ApiKey { user: String, key: String },
    /// `u`, `p=enc:<hex>`, for servers that refuse tokens (LDAP back ends).
    Password { user: String, hex: String },
}

impl Auth {
    /// Token authentication with a fresh salt.
    pub fn token(user: &str, password: &str) -> Auth {
        let salt = crate::random_hex(8);
        Auth::Token {
            user: user.to_string(),
            token: md5_hex(&format!("{password}{salt}")),
            salt,
        }
    }

    pub fn password(user: &str, password: &str) -> Auth {
        Auth::Password {
            user: user.to_string(),
            hex: password.bytes().map(|b| format!("{b:02x}")).collect(),
        }
    }

    pub fn user(&self) -> &str {
        match self {
            Auth::Token { user, .. } | Auth::ApiKey { user, .. } | Auth::Password { user, .. } => {
                user
            }
        }
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        match self {
            Auth::Token { user, salt, token } => {
                vec![
                    ("u", user.clone()),
                    ("t", token.clone()),
                    ("s", salt.clone()),
                ]
            }
            Auth::ApiKey { key, .. } => vec![("apiKey", key.clone())],
            Auth::Password { user, hex } => {
                vec![("u", user.clone()), ("p", format!("enc:{hex}"))]
            }
        }
    }
}

pub fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A signed-in user on one server.
#[derive(Clone, Debug)]
pub struct Session {
    pub server: String,
    pub auth: Auth,
    /// "Navidrome 0.58.0", or empty when the server does not say.
    pub server_name: String,
}

impl Session {
    pub fn to_json(&self) -> Value {
        let auth = match &self.auth {
            Auth::Token { user, salt, token } => {
                json!({"method": "token", "user": user, "salt": salt, "token": token})
            }
            Auth::ApiKey { user, key } => json!({"method": "api_key", "user": user, "key": key}),
            Auth::Password { user, hex } => {
                json!({"method": "password", "user": user, "hex": hex})
            }
        };
        json!({"server": self.server, "server_name": self.server_name, "auth": auth})
    }

    pub fn from_json(v: &Value) -> Option<Session> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let a = &v["auth"];
        let user = s(a, "user").unwrap_or_default();
        let auth = match a["method"].as_str()? {
            "token" => Auth::Token {
                user,
                salt: s(a, "salt")?,
                token: s(a, "token")?,
            },
            "api_key" => Auth::ApiKey {
                user,
                key: s(a, "key")?,
            },
            "password" => Auth::Password {
                user,
                hex: s(a, "hex")?,
            },
            _ => return None,
        };
        Some(Session {
            server: s(v, "server")?,
            server_name: s(v, "server_name").unwrap_or_default(),
            auth,
        })
    }

    /// `<server>/rest/<endpoint>?<auth>&v&c[&f=json]&<extra>`, for URLs the
    /// host fetches itself (streams, cover art).
    pub fn url(&self, endpoint: &str, extra: &[(&str, &str)]) -> String {
        let mut q: Vec<(String, String)> = self
            .auth
            .params()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        q.push(("v".into(), API.into()));
        q.push(("c".into(), CLIENT.into()));
        q.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let query: Vec<String> = q
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect();
        format!("{}/rest/{endpoint}?{}", self.server, query.join("&"))
    }

    pub fn cover(&self, id: &str) -> String {
        self.url("getCoverArt", &[("id", id), ("size", "600")])
    }
}

/// Percent-encode a query value (RFC 3986 unreserved characters stay).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Debug)]
pub enum Error {
    /// Wrong credentials, or a revoked API key (errors 40, 44; HTTP 401).
    Auth,
    /// The server does not take this way of signing in (41, 42).
    Method(String),
    /// Error 70, or HTTP 404.
    NotFound,
    /// The server said no for another reason.
    Status(u16, String),
    /// DNS, TCP, TLS, timeouts, answers that are not the Subsonic API.
    Network(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Auth => write!(f, "wrong user name, password or API key"),
            Error::Method(m) => write!(f, "{m}"),
            Error::NotFound => write!(f, "not found"),
            Error::Status(code, msg) if msg.is_empty() => write!(f, "server answered {code}"),
            Error::Status(code, msg) => write!(f, "server answered {code}: {msg}"),
            Error::Network(e) => write!(f, "{e}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `https://host:4533/navidrome/` → `https://host:4533/navidrome`; a bare
/// host gets `http://`. A trailing `/rest` (a common paste) is dropped.
pub fn normalize_server(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    let (scheme, rest) = match s.split_once("://") {
        Some((sc @ ("http" | "https"), rest)) => (sc, rest),
        Some(_) => return None,
        None => ("http", s),
    };
    let mut rest = rest.trim_end_matches('/');
    if let Some(r) = rest.strip_suffix("/rest") {
        rest = r.trim_end_matches('/');
    }
    (!rest.is_empty() && !rest.starts_with('/')).then(|| format!("{scheme}://{rest}"))
}

/// The body of a `subsonic-response`, or its error.
fn unwrap(v: Value) -> Result<Value> {
    let Some(r) = v.get("subsonic-response") else {
        return Err(Error::Network("not a Subsonic server".into()));
    };
    if r["status"] == "ok" {
        return Ok(r.clone());
    }
    let code = r["error"]["code"].as_u64().unwrap_or(0);
    let msg = r["error"]["message"].as_str().unwrap_or("").to_string();
    Err(match code {
        40 | 44 => Error::Auth,
        41..=43 => Error::Method(msg),
        70 => Error::NotFound,
        50 => Error::Status(403, msg),
        _ => Error::Status(400, format!("error {code} {msg}").trim().to_string()),
    })
}

pub struct Client {
    agent: ureq::Agent,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Client {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .user_agent(concat!("ricercar-subsonic/", env!("CARGO_PKG_VERSION")))
            .build();
        Client { agent }
    }

    fn call(
        &self,
        server: &str,
        endpoint: &str,
        auth: Option<&Auth>,
        query: &[(&str, String)],
    ) -> Result<Value> {
        self.request(server, endpoint, auth, query, None)
    }

    /// GET, or POST with a JSON body (OpenSubsonic endpoints that take one).
    fn request(
        &self,
        server: &str,
        endpoint: &str,
        auth: Option<&Auth>,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let method = if body.is_some() { "POST" } else { "GET" };
        let mut req = self
            .agent
            .request(method, &format!("{server}/rest/{endpoint}"))
            .set("Accept", "application/json")
            .query("v", API)
            .query("c", CLIENT)
            .query("f", "json");
        for (k, v) in auth.map(Auth::params).unwrap_or_default() {
            req = req.query(k, &v);
        }
        for (k, v) in query {
            req = req.query(k, v);
        }
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match resp {
            Ok(r) => {
                let text = r.into_string().map_err(|e| Error::Network(e.to_string()))?;
                let v = serde_json::from_str(&text)
                    .map_err(|_| Error::Network("not a Subsonic server".into()))?;
                unwrap(v)
            }
            // Some servers also say it with the HTTP status, and a JSON body.
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                match serde_json::from_str::<Value>(&text).map(unwrap) {
                    Ok(Err(e)) => Err(e),
                    _ if code == 401 => Err(Error::Auth),
                    _ if code == 404 => Err(Error::NotFound),
                    _ => {
                        let msg: String = text.chars().take(200).collect();
                        Err(Error::Status(code, msg.trim().to_string()))
                    }
                }
            }
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    // ------------------------------------------------------------ sign-in

    /// What the server says about itself before anyone signs in:
    /// `{name, open_subsonic, api_key}`. Fails when the address does not
    /// point at a Subsonic server.
    pub fn probe(&self, server: &str) -> Result<Value> {
        // OpenSubsonic makes this one public.
        let ext = self.call(server, "getOpenSubsonicExtensions", None, &[]);
        let r = match ext {
            Ok(r) => r,
            // Unauthenticated ping: fails (missing credentials), but the
            // answer is a Subsonic one and often names the server.
            Err(Error::Network(e)) => return Err(Error::Network(e)),
            Err(_) => match self.raw_ping(server) {
                Some(r) => r,
                None => return Err(Error::Network("not a Subsonic server".into())),
            },
        };
        let api_key = r["openSubsonicExtensions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["name"] == "apiKeyAuthentication"));
        Ok(json!({
            "name": server_name(&r),
            "open_subsonic": r["openSubsonic"].as_bool().unwrap_or(false),
            "api_key": api_key,
        }))
    }

    fn raw_ping(&self, server: &str) -> Option<Value> {
        let r = self
            .agent
            .get(&format!("{server}/rest/ping"))
            .query("v", API)
            .query("c", CLIENT)
            .query("f", "json")
            .call();
        let text = match r {
            Ok(r) => r.into_string().ok()?,
            Err(ureq::Error::Status(_, r)) => r.into_string().ok()?,
            Err(_) => return None,
        };
        serde_json::from_str::<Value>(&text)
            .ok()?
            .get("subsonic-response")
            .cloned()
    }

    /// Check credentials with `ping`, and name the account.
    pub fn sign_in(&self, server: &str, auth: Auth) -> Result<Session> {
        let r = self.call(server, "ping", Some(&auth), &[])?;
        let auth = match auth {
            // An API key does not name its user; `tokenInfo` does.
            Auth::ApiKey { key, user } if user.is_empty() => {
                let bare = Auth::ApiKey {
                    key: key.clone(),
                    user,
                };
                let user = self
                    .call(server, "tokenInfo", Some(&bare), &[])
                    .ok()
                    .and_then(|v| v["tokenInfo"]["username"].as_str().map(str::to_string))
                    .unwrap_or_default();
                Auth::ApiKey { key, user }
            }
            a => a,
        };
        Ok(Session {
            server: server.to_string(),
            auth,
            server_name: server_name(&r),
        })
    }

    /// Password sign-in: a salted token, or the password itself when the
    /// server does not take tokens.
    pub fn sign_in_password(&self, server: &str, user: &str, password: &str) -> Result<Session> {
        match self.sign_in(server, Auth::token(user, password)) {
            Err(Error::Method(_)) => self.sign_in(server, Auth::password(user, password)),
            r => r,
        }
    }

    // ------------------------------------------------------------ queries

    pub fn get(&self, s: &Session, endpoint: &str, query: &[(&str, String)]) -> Result<Value> {
        self.call(&s.server, endpoint, Some(&s.auth), query)
    }

    pub fn post(
        &self,
        s: &Session,
        endpoint: &str,
        query: &[(&str, String)],
        body: &Value,
    ) -> Result<Value> {
        self.request(&s.server, endpoint, Some(&s.auth), query, Some(body))
    }
}

/// "Navidrome 0.58.0" from `type` and `serverVersion` (OpenSubsonic).
fn server_name(r: &Value) -> String {
    let mut kind = r["type"].as_str().unwrap_or("").to_string();
    if let Some(c) = kind.get(..1) {
        kind = c.to_uppercase() + &kind[1..];
    }
    let v = r["serverVersion"].as_str().unwrap_or("");
    format!("{kind} {v}").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(
            normalize_server("nd.lan:4533").as_deref(),
            Some("http://nd.lan:4533")
        );
        assert_eq!(
            normalize_server(" https://x.org/music/rest/ ").as_deref(),
            Some("https://x.org/music")
        );
        assert_eq!(normalize_server("ftp://x"), None);
        assert_eq!(normalize_server("http://"), None);
        assert_eq!(normalize_server("a b"), None);
    }

    #[test]
    fn token() {
        // The example of the Subsonic API documentation.
        assert_eq!(md5_hex("sesamec19b2d"), "26719a1196d2a940705a59634eb18eab");
        let Auth::Token { salt, token, .. } = Auth::token("u", "sesame") else {
            panic!()
        };
        assert_eq!(token, md5_hex(&format!("sesame{salt}")));
    }

    #[test]
    fn sessions_round_trip() {
        for auth in [
            Auth::token("ann", "pw"),
            Auth::ApiKey {
                user: "ann".into(),
                key: "k1".into(),
            },
            Auth::password("ann", "pw"),
        ] {
            let s = Session {
                server: "http://nd".into(),
                auth,
                server_name: "Navidrome 0.58.0".into(),
            };
            let back = Session::from_json(&s.to_json()).unwrap();
            assert_eq!(back.auth, s.auth);
            assert_eq!(back.server, s.server);
        }
        assert_eq!(Auth::password("a", "pw").params()[1].1, "enc:7077");
    }

    #[test]
    fn urls() {
        let s = Session {
            server: "http://nd".into(),
            auth: Auth::ApiKey {
                user: "a".into(),
                key: "k&1".into(),
            },
            server_name: String::new(),
        };
        assert_eq!(
            s.url("stream", &[("id", "a b"), ("format", "raw")]),
            "http://nd/rest/stream?apiKey=k%261&v=1.16.1&c=ricercar&id=a%20b&format=raw"
        );
    }

    #[test]
    fn errors() {
        let e = |code: u64| {
            unwrap(
                json!({"subsonic-response": {"status": "failed", "error": {"code": code, "message": "m"}}}),
            )
        };
        assert!(matches!(e(40), Err(Error::Auth)));
        assert!(matches!(e(41), Err(Error::Method(_))));
        assert!(matches!(e(70), Err(Error::NotFound)));
        assert!(matches!(unwrap(json!({"x": 1})), Err(Error::Network(_))));
        let ok = unwrap(
            json!({"subsonic-response": {"status": "ok", "type": "navidrome", "serverVersion": "0.58.0"}}),
        );
        assert_eq!(server_name(&ok.unwrap()), "Navidrome 0.58.0");
    }
}
