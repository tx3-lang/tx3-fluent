//! Sign-in with the MCP resource's OIDC issuer: OAuth 2.1 authorization code
//! with PKCE (S256), as a confidential client.
//!
//! The issuer's endpoints come from its discovery document,
//! `{issuer}/.well-known/openid-configuration`, fetched on the first sign-in
//! and kept for the life of the process; a failed fetch is tried again on the
//! next sign-in. The client authenticates to the token endpoint with
//! `client_secret_post` when the issuer offers it, as Auth0 applications do
//! by default, and with `client_secret_basic` otherwise. The signed-in
//! subject is the userinfo `sub`, the same subject the issuer's access tokens
//! carry to `/mcp`.

use std::time::Duration;

use anyhow::Context;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use serde::Deserialize;
use tokio::sync::OnceCell;

use super::session::{LOGIN_TTL, PendingLogin, expires_in};

/// How long one request to the issuer may take.
const ISSUER_TIMEOUT: Duration = Duration::from_secs(10);

/// The scopes requested: the subject, and the email and name the pages show.
const SCOPES: [&str; 3] = ["openid", "email", "profile"];

/// A client with its authorization, token and redirect URLs set.
type Client = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// The sign-in client of one issuer.
pub struct Oidc {
    issuer: String,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect: RedirectUrl,
    http: reqwest::Client,
    endpoints: OnceCell<Endpoints>,
}

/// The endpoints the discovery document names.
#[derive(Debug, Clone)]
struct Endpoints {
    authorize: AuthUrl,
    token: TokenUrl,
    userinfo: String,
    auth_type: AuthType,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    /// OpenID Connect Discovery §3: `client_secret_basic` when absent.
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// Who signed in, as the issuer's userinfo endpoint says.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Identity {
    /// The subject.
    pub sub: String,
    /// The user's email, when the issuer shares it.
    #[serde(default)]
    pub email: Option<String>,
    /// The user's name, when the issuer shares it.
    #[serde(default)]
    pub name: Option<String>,
}

impl Identity {
    /// What the pages call the user: their email, else their name, else the
    /// subject.
    pub fn display(&self) -> String {
        [&self.email, &self.name]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or(&self.sub)
            .clone()
    }
}

impl Oidc {
    /// A client of `issuer` registered as `client_id` with `client_secret`,
    /// redirecting to `redirect_url`.
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        redirect_url: &str,
    ) -> anyhow::Result<Oidc> {
        let http = reqwest::Client::builder()
            .timeout(ISSUER_TIMEOUT)
            // OAuth endpoints must not be followed elsewhere (RFC 9700 §4.11).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the sign-in HTTP client")?;
        Ok(Oidc {
            issuer: issuer.to_string(),
            client_id: ClientId::new(client_id.to_string()),
            client_secret: ClientSecret::new(client_secret.to_string()),
            redirect: RedirectUrl::new(redirect_url.to_string())
                .context("site.redirect_url is not a URL")?,
            http,
            endpoints: OnceCell::new(),
        })
    }

    /// The issuer's endpoints, discovered once.
    async fn endpoints(&self) -> anyhow::Result<&Endpoints> {
        self.endpoints.get_or_try_init(|| self.discover()).await
    }

    async fn discover(&self) -> anyhow::Result<Endpoints> {
        let issuer = self.issuer.trim_end_matches('/');
        let url = format!("{issuer}/.well-known/openid-configuration");
        let discovery: Discovery = self
            .http
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("fetching {url}"))?
            .json()
            .await
            .with_context(|| format!("reading {url}"))?;
        // OpenID Connect Discovery §4.3: the document must name the issuer
        // it was fetched from.
        if discovery.issuer.trim_end_matches('/') != issuer {
            anyhow::bail!(
                "{url} names issuer {}, not {}",
                discovery.issuer,
                self.issuer
            );
        }
        Ok(Endpoints {
            authorize: AuthUrl::new(discovery.authorization_endpoint)
                .context("the authorization endpoint is not a URL")?,
            token: TokenUrl::new(discovery.token_endpoint)
                .context("the token endpoint is not a URL")?,
            userinfo: discovery.userinfo_endpoint,
            auth_type: auth_type(&discovery.token_endpoint_auth_methods_supported),
        })
    }

    fn client(&self, endpoints: &Endpoints) -> Client {
        BasicClient::new(self.client_id.clone())
            .set_client_secret(self.client_secret.clone())
            .set_auth_uri(endpoints.authorize.clone())
            .set_token_uri(endpoints.token.clone())
            .set_redirect_uri(self.redirect.clone())
            .set_auth_type(endpoints.auth_type.clone())
    }

    /// Starts a sign-in: the issuer URL to send the browser to, and what the
    /// callback needs to finish it.
    pub async fn begin(&self) -> anyhow::Result<(String, PendingLogin)> {
        let endpoints = self.endpoints().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state) = self
            .client(endpoints)
            .authorize_url(CsrfToken::new_random)
            .add_scopes(SCOPES.map(|s| Scope::new(s.to_string())))
            .set_pkce_challenge(challenge)
            .url();
        let pending = PendingLogin {
            state: state.secret().clone(),
            verifier: verifier.secret().clone(),
            expires_at: expires_in(LOGIN_TTL),
        };
        Ok((url.to_string(), pending))
    }

    /// Redeems the authorization `code` with the sign-in's PKCE `verifier`
    /// and asks the userinfo endpoint who signed in.
    pub async fn finish(&self, code: String, verifier: String) -> anyhow::Result<Identity> {
        let endpoints = self.endpoints().await?;
        let token = self
            .client(endpoints)
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier))
            .request_async(&self.http)
            .await
            .map_err(|err| anyhow::anyhow!("redeeming the authorization code: {err}"))?;
        let identity: Identity = self
            .http
            .get(&endpoints.userinfo)
            .bearer_auth(token.access_token().secret())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("fetching userinfo")?
            .json()
            .await
            .context("reading userinfo")?;
        if identity.sub.is_empty() {
            anyhow::bail!("userinfo has an empty sub");
        }
        Ok(identity)
    }
}

/// How the client authenticates, given the methods the issuer supports.
fn auth_type(supported: &[String]) -> AuthType {
    if supported.iter().any(|m| m == "client_secret_post") {
        AuthType::RequestBody
    } else {
        AuthType::BasicAuth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_secret_post_is_preferred_when_offered() {
        let methods = |m: &[&str]| m.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(
            auth_type(&methods(&["client_secret_basic", "client_secret_post"])),
            AuthType::RequestBody
        ));
        assert!(matches!(
            auth_type(&methods(&["client_secret_basic"])),
            AuthType::BasicAuth
        ));
        assert!(matches!(auth_type(&[]), AuthType::BasicAuth));
    }

    #[test]
    fn identities_display_email_then_name_then_subject() {
        let mut identity = Identity {
            sub: "auth0|1".into(),
            email: Some("a@x".into()),
            name: Some("Alice".into()),
        };
        assert_eq!(identity.display(), "a@x");
        identity.email = None;
        assert_eq!(identity.display(), "Alice");
        identity.name = Some(String::new());
        assert_eq!(identity.display(), "auth0|1");
    }
}
