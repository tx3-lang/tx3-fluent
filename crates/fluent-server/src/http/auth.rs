//! Caller authentication for the HTTP transport, selected by `[auth].mode`.
//!
//! - `none` accepts every caller; the server refuses to bind anything but a
//!   loopback address in this mode.
//! - `token` accepts `Authorization: Bearer <token>` when `<token>` equals the
//!   value of `token_env`, compared in constant time.
//! - `oidc` accepts a JWT signed with RS256 or ES256 by a key from the
//!   issuer's JWKS, whose `iss` is the issuer, whose `aud` contains the
//!   audience, and whose `exp` and `nbf` hold (with a minute of leeway).
//!
//! A rejected request gets `401` with an empty body and
//! `WWW-Authenticate: Bearer resource_metadata="…"` naming the
//! [protected resource metadata](ProtectedResourceMetadata) when the server
//! has a public URL. An accepted request carries its [`Principal`] in its
//! extensions and no longer carries its `Authorization` header, so nothing
//! downstream can log the credential.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fluent_core::config::AuthConfig;
use http::{HeaderValue, StatusCode, header};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, JwkSet, KeyAlgorithm};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock};

/// Path of the RFC 9728 protected resource metadata document.
pub const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// How long fetched keys are trusted before the JWKS is fetched again.
pub const JWKS_REFRESH: Duration = Duration::from_secs(10 * 60);

/// The least time between two fetches caused by unknown `kid`s, so a stream of
/// forged tokens cannot turn into a stream of requests to the issuer.
pub const JWKS_MIN_REFETCH: Duration = Duration::from_secs(1);

/// The longest a JWKS fetch may take.
const JWKS_TIMEOUT: Duration = Duration::from_secs(10);

/// The subject every caller of a `token`-mode server shares.
pub const TOKEN_SUBJECT: &str = "token";

/// Who made a request, as authentication established it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// The token's `sub` claim; [`TOKEN_SUBJECT`] in `token` mode.
    pub sub: String,
    /// The token's `email` claim, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Why a request was rejected. Logged, never sent to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No `Authorization: Bearer` header.
    Missing,
    /// A bearer token that does not match, or a JWT that fails validation.
    Invalid,
    /// A JWT whose `kid` names no key in the issuer's JWKS.
    UnknownKey,
    /// The issuer's JWKS could not be fetched.
    KeysUnavailable,
}

impl Rejection {
    fn as_str(self) -> &'static str {
        match self {
            Rejection::Missing => "missing bearer token",
            Rejection::Invalid => "invalid bearer token",
            Rejection::UnknownKey => "unknown signing key",
            Rejection::KeysUnavailable => "signing keys unavailable",
        }
    }
}

/// Authenticates requests in the configured mode.
pub struct Authenticator {
    mode: Mode,
    challenge: HeaderValue,
    metadata: Option<ProtectedResourceMetadata>,
}

enum Mode {
    None,
    Token(Vec<u8>),
    Oidc(Box<OidcVerifier>),
}

impl Authenticator {
    /// The authenticator for `auth`, advertising metadata under `public_url`
    /// (without a trailing slash).
    ///
    /// Fails when `token` mode's variable is unset or empty, or `oidc` mode
    /// has no public URL to advertise.
    pub fn new(auth: &AuthConfig, public_url: Option<&str>) -> anyhow::Result<Authenticator> {
        let public_url = public_url.map(|url| url.trim_end_matches('/'));
        let challenge = match public_url {
            Some(url) => format!("Bearer resource_metadata=\"{url}{METADATA_PATH}\""),
            None => "Bearer".to_string(),
        };
        let challenge = HeaderValue::from_str(&challenge)?;
        let (mode, metadata) = match auth {
            AuthConfig::None {} => (Mode::None, None),
            AuthConfig::Token { token_env } => {
                let token = token_env.value().unwrap_or_default();
                anyhow::ensure!(
                    !token.is_empty(),
                    "auth.token_env names {}, which is unset or empty",
                    token_env.var()
                );
                (Mode::Token(token.as_bytes().to_vec()), None)
            }
            AuthConfig::Oidc {
                issuer,
                jwks_url,
                audience,
            } => {
                let Some(public_url) = public_url else {
                    anyhow::bail!("auth.mode = \"oidc\" requires server.public_url");
                };
                let metadata = ProtectedResourceMetadata {
                    resource: format!("{public_url}/mcp"),
                    authorization_servers: vec![issuer.clone()],
                    bearer_methods_supported: vec!["header".to_string()],
                    scopes_supported: Vec::new(),
                };
                let verifier = OidcVerifier::new(issuer, jwks_url, audience)?;
                (Mode::Oidc(Box::new(verifier)), Some(metadata))
            }
        };
        Ok(Authenticator {
            mode,
            challenge,
            metadata,
        })
    }

    /// Whether every caller is accepted.
    pub fn is_open(&self) -> bool {
        matches!(self.mode, Mode::None)
    }

    /// The protected resource metadata; `oidc` mode only.
    pub fn metadata(&self) -> Option<&ProtectedResourceMetadata> {
        self.metadata.as_ref()
    }

    /// The principal behind `authorization`, the request's `Authorization`
    /// header; `None` in `none` mode.
    pub async fn authenticate(
        &self,
        authorization: Option<&HeaderValue>,
    ) -> Result<Option<Principal>, Rejection> {
        if let Mode::None = self.mode {
            return Ok(None);
        }
        let token = authorization
            .and_then(|value| value.to_str().ok())
            .and_then(bearer)
            .ok_or(Rejection::Missing)?;
        match &self.mode {
            Mode::None => Ok(None),
            Mode::Token(expected) => {
                if bool::from(token.as_bytes().ct_eq(expected)) {
                    Ok(Some(Principal {
                        sub: TOKEN_SUBJECT.to_string(),
                        email: None,
                    }))
                } else {
                    Err(Rejection::Invalid)
                }
            }
            Mode::Oidc(verifier) => verifier.verify(token).await.map(Some),
        }
    }

    /// The `401` a rejected request gets.
    pub fn unauthorized(&self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, self.challenge.clone())],
        )
            .into_response()
    }
}

/// The token of a `Bearer` credential; the scheme is case-insensitive.
fn bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// Middleware: authenticates the request, then passes it on with its
/// [`Principal`] in its extensions and without its `Authorization` header.
pub async fn require_auth(
    State(auth): State<Arc<Authenticator>>,
    mut request: Request,
    next: Next,
) -> Response {
    let result = auth
        .authenticate(request.headers().get(header::AUTHORIZATION))
        .await;
    request.headers_mut().remove(header::AUTHORIZATION);
    match result {
        Ok(principal) => {
            if let Some(principal) = principal {
                request.extensions_mut().insert(principal);
            }
            next.run(request).await
        }
        Err(rejection) => {
            tracing::info!(reason = rejection.as_str(), "request rejected");
            auth.unauthorized()
        }
    }
}

/// The RFC 9728 protected resource metadata document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// The MCP endpoint: `{public_url}/mcp`.
    pub resource: String,
    /// The OIDC issuer.
    pub authorization_servers: Vec<String>,
    /// Always `["header"]`.
    pub bearer_methods_supported: Vec<String>,
    /// Always empty.
    pub scopes_supported: Vec<String>,
}

/// The claims Fluent reads; `jsonwebtoken` checks the registered ones.
#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
}

/// Validates JWTs against an issuer's cached JWKS.
struct OidcVerifier {
    issuer: String,
    audience: String,
    jwks_url: String,
    client: reqwest::Client,
    keys: RwLock<Keys>,
    /// Held while fetching, so concurrent misses fetch once.
    fetching: Mutex<()>,
}

#[derive(Default)]
struct Keys {
    by_kid: HashMap<String, (Algorithm, DecodingKey)>,
    fetched: Option<Instant>,
}

impl Keys {
    fn fresh(&self) -> bool {
        self.fetched
            .is_some_and(|fetched| fetched.elapsed() < JWKS_REFRESH)
    }
}

impl OidcVerifier {
    fn new(issuer: &str, jwks_url: &str, audience: &str) -> anyhow::Result<OidcVerifier> {
        let client = reqwest::Client::builder().timeout(JWKS_TIMEOUT).build()?;
        Ok(OidcVerifier {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            jwks_url: jwks_url.to_string(),
            client,
            keys: RwLock::new(Keys::default()),
            fetching: Mutex::new(()),
        })
    }

    async fn verify(&self, token: &str) -> Result<Principal, Rejection> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| Rejection::Invalid)?;
        let kid = header.kid.ok_or(Rejection::Invalid)?;
        let (algorithm, key) = self.key(&kid).await?;
        if header.alg != algorithm {
            return Err(Rejection::Invalid);
        }

        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|_| Rejection::Invalid)?
            .claims;
        Ok(Principal {
            sub: claims.sub,
            email: claims.email,
        })
    }

    /// The key `kid` names, fetching the JWKS when the cache is stale or does
    /// not know `kid`.
    async fn key(&self, kid: &str) -> Result<(Algorithm, DecodingKey), Rejection> {
        {
            let keys = self.keys.read().await;
            if keys.fresh()
                && let Some(key) = keys.by_kid.get(kid)
            {
                return Ok(key.clone());
            }
        }

        let _fetching = self.fetching.lock().await;
        {
            // Another request may have fetched while this one waited.
            let keys = self.keys.read().await;
            let recent = keys
                .fetched
                .is_some_and(|fetched| fetched.elapsed() < JWKS_MIN_REFETCH);
            if keys.fresh() && (recent || keys.by_kid.contains_key(kid)) {
                return keys.by_kid.get(kid).cloned().ok_or(Rejection::UnknownKey);
            }
        }

        match self.fetch().await {
            Ok(by_kid) => {
                let mut keys = self.keys.write().await;
                *keys = Keys {
                    by_kid,
                    fetched: Some(Instant::now()),
                };
                keys.by_kid.get(kid).cloned().ok_or(Rejection::UnknownKey)
            }
            Err(err) => {
                tracing::warn!(error = %err, "fetching the issuer's JWKS failed");
                // Keep serving the last keys until the issuer answers again.
                let keys = self.keys.read().await;
                keys.by_kid
                    .get(kid)
                    .cloned()
                    .ok_or(Rejection::KeysUnavailable)
            }
        }
    }

    /// The usable keys of the issuer's JWKS, by `kid`: RSA keys for RS256 and
    /// P-256 keys for ES256. Keys without a `kid`, or marked for another
    /// algorithm, are skipped.
    async fn fetch(&self) -> anyhow::Result<HashMap<String, (Algorithm, DecodingKey)>> {
        let set: JwkSet = self
            .client
            .get(&self.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut keys = HashMap::new();
        for jwk in &set.keys {
            let Some(kid) = &jwk.common.key_id else {
                continue;
            };
            let (algorithm, key_algorithm) = match &jwk.algorithm {
                AlgorithmParameters::RSA(_) => (Algorithm::RS256, KeyAlgorithm::RS256),
                AlgorithmParameters::EllipticCurve(ec) if ec.curve == EllipticCurve::P256 => {
                    (Algorithm::ES256, KeyAlgorithm::ES256)
                }
                _ => continue,
            };
            if jwk
                .common
                .key_algorithm
                .is_some_and(|declared| declared != key_algorithm)
            {
                continue;
            }
            if let Ok(key) = DecodingKey::from_jwk(jwk) {
                keys.insert(kid.clone(), (algorithm, key));
            }
        }
        tracing::info!(keys = keys.len(), "fetched the issuer's JWKS");
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parses_the_scheme_case_insensitively() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("Bearer"), None);
    }
}
