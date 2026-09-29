//! Signed cookies: the seven-day session of a signed-in user and the
//! ten-minute record of a sign-in in progress.
//!
//! Both are HTTP-only, `SameSite=Lax`, `Secure` when the site is served over
//! HTTPS, and signed with a key derived from `[site].session_secret_env`. A
//! value is JSON, hex-encoded so it stays a valid cookie value, and carries
//! its own expiry, so a replayed cookie stops working when it would have
//! expired.

use std::time::Duration;

use axum::response::IntoResponse;
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use http::HeaderMap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::store::now;

/// The session cookie's name.
pub const SESSION_COOKIE: &str = "fluent_session";

/// The pending sign-in cookie's name; sent only to `/auth`.
pub const LOGIN_COOKIE: &str = "fluent_login";

/// How long a session lasts.
pub const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long a sign-in may take between `/auth/login` and `/auth/callback`.
pub const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

/// The shortest session secret accepted, in bytes.
pub const MIN_SECRET_LEN: usize = 32;

/// A signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The OIDC subject: the store user.
    pub sub: String,
    /// What the pages call the user: their email, else their name, else the
    /// subject.
    pub display: String,
    /// The token every form of this session must carry.
    pub csrf: String,
    /// When the session ends, in Unix seconds.
    pub expires_at: i64,
}

/// A sign-in in progress: the state sent to the issuer and the PKCE verifier
/// that redeems its code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLogin {
    /// The `state` parameter the callback must return.
    pub state: String,
    /// The PKCE code verifier.
    pub verifier: String,
    /// When the sign-in expires, in Unix seconds.
    pub expires_at: i64,
}

/// A cookie value that expires.
pub trait Expiring {
    /// When it expires, in Unix seconds.
    fn expires_at(&self) -> i64;
}

impl Expiring for Session {
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

impl Expiring for PendingLogin {
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

/// Reads and writes the site's signed cookies.
#[derive(Clone)]
pub struct Cookies {
    key: Key,
    secure: bool,
}

impl Cookies {
    /// Signs with a key derived from `secret`, which must be at least
    /// [`MIN_SECRET_LEN`] bytes. Cookies are `Secure` when `secure`.
    pub fn new(secret: &str, secure: bool) -> anyhow::Result<Cookies> {
        if secret.len() < MIN_SECRET_LEN {
            anyhow::bail!("the session secret must be at least {MIN_SECRET_LEN} bytes");
        }
        Ok(Cookies {
            key: Key::derive_from(secret.as_bytes()),
            secure,
        })
    }

    /// The signed cookies of a request; unsigned or tampered ones are left
    /// out.
    pub fn jar(&self, headers: &HeaderMap) -> SignedCookieJar {
        SignedCookieJar::from_headers(headers, self.key.clone())
    }

    /// The unexpired value of cookie `name`, when it is present, signed and
    /// well formed.
    pub fn read<T: DeserializeOwned + Expiring>(jar: &SignedCookieJar, name: &str) -> Option<T> {
        let cookie = jar.get(name)?;
        let bytes = hex::decode(cookie.value()).ok()?;
        let value: T = serde_json::from_slice(&bytes).ok()?;
        (value.expires_at() > now()).then_some(value)
    }

    /// `jar` with cookie `name` set to `value`, for `ttl`, sent to `path`.
    pub fn write<T: Serialize>(
        &self,
        jar: SignedCookieJar,
        name: &'static str,
        path: &'static str,
        value: &T,
        ttl: Duration,
    ) -> SignedCookieJar {
        // Serializing these plain structs cannot fail.
        let json = serde_json::to_vec(value).unwrap_or_default();
        let max_age = cookie_duration(ttl);
        jar.add(
            Cookie::build((name, hex::encode(json)))
                .path(path)
                .http_only(true)
                .same_site(SameSite::Lax)
                .secure(self.secure)
                .max_age(max_age),
        )
    }

    /// `jar` with cookie `name`, sent to `path`, removed.
    pub fn remove(jar: SignedCookieJar, name: &'static str, path: &'static str) -> SignedCookieJar {
        jar.remove(Cookie::build(name).path(path))
    }

    /// A `Cookie` request header carrying `value` as cookie `name`, as a
    /// browser would send it back after a sign-in; for tests.
    pub fn header_for<T: Serialize>(&self, name: &'static str, value: &T) -> String {
        let jar = self.write(
            SignedCookieJar::new(self.key.clone()),
            name,
            "/",
            value,
            SESSION_TTL,
        );
        let response = (jar, ()).into_response();
        response
            .headers()
            .get(http::header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or_default()
            .to_string()
    }
}

/// `ttl` as a cookie `Max-Age`.
fn cookie_duration(ttl: Duration) -> time::Duration {
    time::Duration::seconds(i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX))
}

/// Unix seconds `ttl` from now.
pub fn expires_in(ttl: Duration) -> i64 {
    now().saturating_add(i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn session(expires_at: i64) -> Session {
        Session {
            sub: "alice".into(),
            display: "alice@example.com".into(),
            csrf: "token".into(),
            expires_at,
        }
    }

    fn headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::COOKIE, cookie.parse().unwrap());
        headers
    }

    #[test]
    fn short_secrets_are_refused() {
        assert!(Cookies::new("short", false).is_err());
    }

    #[test]
    fn a_signed_session_reads_back() {
        let cookies = Cookies::new(SECRET, false).unwrap();
        let value = session(expires_in(SESSION_TTL));
        let header = cookies.header_for(SESSION_COOKIE, &value);
        let jar = cookies.jar(&headers(&header));
        assert_eq!(Cookies::read::<Session>(&jar, SESSION_COOKIE), Some(value));
    }

    #[test]
    fn expired_tampered_and_foreign_sessions_are_ignored() {
        let cookies = Cookies::new(SECRET, false).unwrap();
        let expired = cookies.header_for(SESSION_COOKIE, &session(now() - 1));
        assert_eq!(
            Cookies::read::<Session>(&cookies.jar(&headers(&expired)), SESSION_COOKIE),
            None
        );

        let unsigned = format!(
            "{SESSION_COOKIE}={}",
            hex::encode(serde_json::to_vec(&session(expires_in(SESSION_TTL))).unwrap())
        );
        assert_eq!(
            Cookies::read::<Session>(&cookies.jar(&headers(&unsigned)), SESSION_COOKIE),
            None
        );

        let other = Cookies::new("another secret, just as long as the first", false).unwrap();
        let foreign = other.header_for(SESSION_COOKIE, &session(expires_in(SESSION_TTL)));
        assert_eq!(
            Cookies::read::<Session>(&cookies.jar(&headers(&foreign)), SESSION_COOKIE),
            None
        );
    }
}
