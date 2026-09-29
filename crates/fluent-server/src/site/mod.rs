//! The companion site: signed-in users choose which registrations their MCP
//! sessions see, and read how to connect ChatGPT.
//!
//! | Route | Signed in | Serves |
//! |---|---|---|
//! | `GET /` | no | The landing page and the sign-in button |
//! | `GET /auth/login` | no | Redirects to the issuer's sign-in |
//! | `GET /auth/callback` | no | Finishes the sign-in, then redirects to `/protocols` |
//! | `POST /auth/logout` | yes | Ends the session, then redirects to `/` |
//! | `GET /protocols` | yes | Every registration, its status and a toggle |
//! | `POST /protocols/{slug}` | yes | `enabled=on\|off`: selects or deselects `slug` |
//! | `GET /connect` | yes | The MCP server URL and the ChatGPT steps |
//! | `GET /site.css` | no | The one stylesheet |
//!
//! Pages are server-rendered [askama] templates without scripts. A signed-in
//! page without a session redirects to `/`; a revoked user is refused with
//! `403`. Every form carries the session's CSRF token, and a POST without it
//! is refused with `403`. Selection changes go through
//! [`Store::set_selection`], which announces them so the user's open MCP
//! sessions get `notifications/tools/list_changed`.
//!
//! The site exists only when `[site].enabled = true`: [`Site::new`] returns
//! `None` otherwise and none of these routes are served.

pub mod oidc;
pub mod session;

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context;
use askama::Template;
use axum::extract::{Form, Path, Query, State};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Router, extract::Request};
use axum_extra::extract::cookie::SignedCookieJar;
use fluent_core::config::AuthConfig;
use fluent_core::{Catalog, Config};
use http::header::{self, HeaderValue};
use http::{HeaderMap, StatusCode};
use oauth2::CsrfToken;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use crate::store::{Select, SelectionStatus, Store};
use oidc::Oidc;
use session::{
    Cookies, LOGIN_COOKIE, PendingLogin, SESSION_COOKIE, SESSION_TTL, Session, expires_in,
};

/// The brand line of the landing page.
pub const BRAND_LINE: &str = "Tx3 Fluent — make your AI assistant fluent in on-chain protocols";

/// What the connect page says to do after changing a selection.
pub const REFRESH_SENTENCE: &str = "After changing your selection, open the connection in ChatGPT \
    and choose Refresh so the new tools appear";

/// What the connect page says Fluent never does.
pub const NEVER_SIGNS_SENTENCE: &str = "Tx3 Fluent never signs or submits transactions";

/// The stylesheet, served at `/site.css`.
const CSS: &str = include_str!("../../static/site.css");

/// Sent with every site response: no scripts, frames or foreign forms, and no
/// caching of per-user pages.
const SECURITY_HEADERS: [(header::HeaderName, &str); 4] = [
    (
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; style-src 'self'; form-action 'self'; \
         frame-ancestors 'none'; base-uri 'none'",
    ),
    (header::X_FRAME_OPTIONS, "DENY"),
    (header::REFERRER_POLICY, "same-origin"),
    (header::CACHE_CONTROL, "no-store"),
];

/// The companion site over one catalog and store.
#[derive(Clone)]
pub struct Site {
    state: Arc<SiteState>,
}

struct SiteState {
    store: Store,
    catalog: Arc<Catalog>,
    cookies: Cookies,
    oidc: Oidc,
    /// The MCP server URL the connect page gives: `{public_url}/mcp`.
    mcp_url: String,
}

impl Site {
    /// The site `config` describes, over `store` and `catalog`; `None` when
    /// `[site].enabled` is false.
    ///
    /// Fails when a secret the site names is unset, when the session secret
    /// is shorter than [`session::MIN_SECRET_LEN`] bytes, or when the
    /// configuration lacks what [`Config::validate`] requires of an enabled
    /// site.
    pub fn new(
        config: &Config,
        store: Store,
        catalog: Arc<Catalog>,
    ) -> anyhow::Result<Option<Site>> {
        let site = &config.site;
        if !site.enabled {
            return Ok(None);
        }
        let AuthConfig::Oidc { issuer, .. } = &config.auth else {
            anyhow::bail!("the site requires auth.mode = \"oidc\"");
        };
        let public_url = config
            .server
            .public_url
            .as_deref()
            .context("the site requires server.public_url")?;
        let redirect_url = site
            .redirect_url
            .as_deref()
            .context("the site requires site.redirect_url")?;
        let secret = |key: &str, secret: &Option<fluent_core::config::SecretRef>| {
            let secret = secret
                .as_ref()
                .with_context(|| format!("the site requires site.{key}"))?;
            secret
                .value()
                .map(str::to_string)
                .with_context(|| format!("site.{key}: {} is unset", secret.var()))
        };
        let session_secret = secret("session_secret_env", &site.session_secret_env)?;
        let client_id = secret("oidc_client_id_env", &site.oidc_client_id_env)?;
        let client_secret = secret("oidc_client_secret_env", &site.oidc_client_secret_env)?;

        // Cookies set over plain HTTP, as on a local run, cannot be `Secure`.
        let secure = redirect_url.starts_with("https://");
        let state = SiteState {
            store,
            catalog,
            cookies: Cookies::new(&session_secret, secure).context("site.session_secret_env")?,
            oidc: Oidc::new(issuer, &client_id, &client_secret, redirect_url)?,
            mcp_url: format!("{}/mcp", public_url.trim_end_matches('/')),
        };
        Ok(Some(Site {
            state: Arc::new(state),
        }))
    }

    /// The site's routes.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(landing))
            .route("/auth/login", get(login))
            .route("/auth/callback", get(callback))
            .route("/auth/logout", post(logout))
            .route("/protocols", get(protocols))
            .route("/protocols/{slug}", post(toggle))
            .route("/connect", get(connect))
            .route("/site.css", get(css))
            .with_state(Arc::clone(&self.state))
            .layer(middleware::from_fn(security_headers))
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    }
    response
}

/// The signed-in user, as the page header shows them.
#[derive(Debug, Clone)]
pub struct UserView {
    /// What the page calls the user.
    pub display: String,
    /// The CSRF token of the sign-out form.
    pub csrf: String,
}

impl From<&Session> for UserView {
    fn from(session: &Session) -> UserView {
        UserView {
            display: session.display.clone(),
            csrf: session.csrf.clone(),
        }
    }
}

#[derive(Template)]
#[template(path = "landing.html")]
struct LandingPage {
    user: Option<UserView>,
    brand_line: &'static str,
}

#[derive(Template)]
#[template(path = "protocols.html")]
struct ProtocolsPage {
    user: Option<UserView>,
    csrf: String,
    rows: Vec<ProtocolRow>,
}

#[derive(Template)]
#[template(path = "connect.html")]
struct ConnectPage {
    user: Option<UserView>,
    mcp_url: String,
    enabled: Vec<ProtocolRow>,
    refresh_sentence: &'static str,
    never_signs_sentence: &'static str,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    user: Option<UserView>,
    title: &'static str,
    message: String,
}

/// One registration on the protocols page.
#[derive(Debug, Clone)]
pub struct ProtocolRow {
    /// The registration slug.
    pub slug: String,
    /// `scope/name`.
    pub protocol: String,
    /// The protocol version.
    pub version: String,
    /// The network it serves.
    pub network: &'static str,
    /// The deployment profile.
    pub profile: String,
    /// The registration's current revision.
    pub revision: String,
    /// Where the user's selection stands.
    pub status: RowStatus,
}

/// A registration's status for one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Selected at the current revision: its tools are in scope.
    Enabled,
    /// Not selected.
    Disabled,
    /// Selected at an earlier revision: its tools are hidden until it is
    /// selected again.
    UpdateRequired,
}

impl RowStatus {
    /// The status as the page writes it.
    pub fn label(&self) -> &'static str {
        match self {
            RowStatus::Enabled => "enabled",
            RowStatus::Disabled => "disabled",
            RowStatus::UpdateRequired => "update required",
        }
    }

    /// The CSS class of the status badge.
    pub fn class(&self) -> &'static str {
        match self {
            RowStatus::Enabled => "enabled",
            RowStatus::Disabled => "disabled",
            RowStatus::UpdateRequired => "update-required",
        }
    }

    /// The toggle buttons: the `enabled` value each submits and its label.
    pub fn actions(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            RowStatus::Enabled => &[("off", "Turn off")],
            RowStatus::Disabled => &[("on", "Turn on")],
            RowStatus::UpdateRequired => &[("on", "Update"), ("off", "Turn off")],
        }
    }
}

/// Renders `page` with `status`.
fn render(status: StatusCode, page: &impl Template) -> Response {
    match page.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(err) => {
            warn!("rendering a site page failed: {err}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}

fn error_page(
    status: StatusCode,
    user: Option<UserView>,
    title: &'static str,
    message: impl Into<String>,
) -> Response {
    render(
        status,
        &ErrorPage {
            user,
            title,
            message: message.into(),
        },
    )
}

/// A redirect to `to` after a form or a sign-in step.
fn see_other(to: &str) -> Redirect {
    Redirect::to(to)
}

impl SiteState {
    /// The request's unexpired session, if any.
    fn session(&self, headers: &HeaderMap) -> (SignedCookieJar, Option<Session>) {
        let jar = self.cookies.jar(headers);
        let session = Cookies::read::<Session>(&jar, SESSION_COOKIE);
        (jar, session)
    }

    /// The session of a signed-in page: without one, a redirect to `/`; for
    /// a revoked user, `403` and the session cookie removed.
    async fn require_session(&self, headers: &HeaderMap) -> Result<Session, Response> {
        let (jar, session) = self.session(headers);
        let Some(session) = session else {
            return Err(see_other("/").into_response());
        };
        match self.store.user(&session.sub).await {
            Ok(Some(user)) if user.is_revoked() => {
                info!("site request refused: the user is revoked");
                let jar = Cookies::remove(jar, SESSION_COOKIE, "/");
                Err((jar, revoked_page()).into_response())
            }
            Ok(_) => Ok(session),
            Err(err) => {
                warn!(code = %err.code(), "reading the site user failed: {err}");
                Err(unavailable_page(Some(UserView::from(&session))))
            }
        }
    }

    /// Every registration and where `sub`'s selection of it stands.
    async fn rows(&self, sub: &str) -> Result<Vec<ProtocolRow>, Response> {
        let selections = self.store.list_selections(sub).await.map_err(|err| {
            warn!(code = %err.code(), "listing the site user's selections failed: {err}");
            unavailable_page(None)
        })?;
        let statuses: BTreeMap<String, SelectionStatus> = selections
            .iter()
            .map(|s| (s.slug.clone(), s.status(&self.catalog)))
            .collect();
        Ok(self
            .catalog
            .iter()
            .map(|registration| {
                let protocol = &registration.manifest().protocol;
                let status = match statuses.get(registration.slug()) {
                    Some(SelectionStatus::Active) => RowStatus::Enabled,
                    Some(SelectionStatus::UpdateRequired) => RowStatus::UpdateRequired,
                    Some(SelectionStatus::Removed) | None => RowStatus::Disabled,
                };
                ProtocolRow {
                    slug: registration.slug().to_string(),
                    protocol: format!("{}/{}", protocol.scope, protocol.name),
                    version: protocol.version.clone(),
                    network: registration.network().as_str(),
                    profile: registration.profile().to_string(),
                    revision: registration.revision().to_string(),
                    status,
                }
            })
            .collect())
    }
}

fn revoked_page() -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        None,
        "Access revoked",
        "This account's access to Tx3 Fluent has been revoked.",
    )
}

fn unavailable_page(user: Option<UserView>) -> Response {
    error_page(
        StatusCode::SERVICE_UNAVAILABLE,
        user,
        "Temporarily unavailable",
        "Tx3 Fluent could not complete the request. Try again shortly.",
    )
}

/// Whether `given` is the session's CSRF token.
fn csrf_matches(session: &Session, given: &str) -> bool {
    !given.is_empty() && bool::from(session.csrf.as_bytes().ct_eq(given.as_bytes()))
}

fn csrf_refused(session: &Session) -> Response {
    info!("site form refused: missing or wrong CSRF token");
    error_page(
        StatusCode::FORBIDDEN,
        Some(UserView::from(session)),
        "Form expired",
        "The form was not accepted. Reload the page and try again.",
    )
}

async fn landing(State(site): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    let (_, session) = site.session(&headers);
    render(
        StatusCode::OK,
        &LandingPage {
            user: session.as_ref().map(UserView::from),
            brand_line: BRAND_LINE,
        },
    )
}

async fn login(State(site): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    match site.oidc.begin().await {
        Ok((url, pending)) => {
            let jar = site.cookies.write(
                site.cookies.jar(&headers),
                LOGIN_COOKIE,
                "/auth",
                &pending,
                session::LOGIN_TTL,
            );
            (jar, Redirect::to(&url)).into_response()
        }
        Err(err) => {
            warn!("starting a sign-in failed: {err:#}");
            error_page(
                StatusCode::BAD_GATEWAY,
                None,
                "Sign-in unavailable",
                "The identity provider could not be reached. Try again shortly.",
            )
        }
    }
}

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(site): State<Arc<SiteState>>,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    let jar = site.cookies.jar(&headers);
    let pending = Cookies::read::<PendingLogin>(&jar, LOGIN_COOKIE);
    let jar = Cookies::remove(jar, LOGIN_COOKIE, "/auth");
    let failed = |status, message: String| {
        (
            jar.clone(),
            error_page(status, None, "Sign-in failed", message),
        )
            .into_response()
    };

    if let Some(error) = params.error {
        info!(%error, "the identity provider refused the sign-in");
        return failed(
            StatusCode::BAD_REQUEST,
            format!("The identity provider refused the sign-in ({error})."),
        );
    }
    let Some(pending) = pending else {
        return failed(
            StatusCode::BAD_REQUEST,
            "The sign-in expired or was started in another browser. Sign in again.".into(),
        );
    };
    let (Some(code), Some(state)) = (params.code, params.state) else {
        return failed(
            StatusCode::BAD_REQUEST,
            "The identity provider's answer was incomplete. Sign in again.".into(),
        );
    };
    if !bool::from(pending.state.as_bytes().ct_eq(state.as_bytes())) {
        info!("sign-in refused: the state does not match");
        return failed(
            StatusCode::BAD_REQUEST,
            "The sign-in could not be verified. Sign in again.".into(),
        );
    }

    let identity = match site.oidc.finish(code, pending.verifier).await {
        Ok(identity) => identity,
        Err(err) => {
            warn!("finishing a sign-in failed: {err:#}");
            return failed(
                StatusCode::BAD_GATEWAY,
                "The identity provider did not confirm the sign-in. Try again.".into(),
            );
        }
    };
    let user = match site
        .store
        .upsert_user(&identity.sub, identity.email.as_deref())
        .await
    {
        Ok(user) => user,
        Err(err) => {
            warn!(code = %err.code(), "recording the signed-in user failed: {err}");
            return (jar, unavailable_page(None)).into_response();
        }
    };
    if user.is_revoked() {
        info!("sign-in refused: the user is revoked");
        return (jar, revoked_page()).into_response();
    }

    let session = Session {
        sub: identity.sub.clone(),
        display: identity.display(),
        csrf: CsrfToken::new_random().secret().clone(),
        expires_at: expires_in(SESSION_TTL),
    };
    info!("user signed in to the site");
    let jar = site
        .cookies
        .write(jar, SESSION_COOKIE, "/", &session, SESSION_TTL);
    (jar, see_other("/protocols")).into_response()
}

/// A form carrying only the CSRF token.
#[derive(Deserialize)]
struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

async fn logout(
    State(site): State<Arc<SiteState>>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Response {
    let (jar, session) = site.session(&headers);
    let Some(session) = session else {
        return see_other("/").into_response();
    };
    if !csrf_matches(&session, &form.csrf) {
        return csrf_refused(&session);
    }
    let jar = Cookies::remove(jar, SESSION_COOKIE, "/");
    (jar, see_other("/")).into_response()
}

async fn protocols(State(site): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    let session = match site.require_session(&headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let rows = match site.rows(&session.sub).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    render(
        StatusCode::OK,
        &ProtocolsPage {
            user: Some(UserView::from(&session)),
            csrf: session.csrf.clone(),
            rows,
        },
    )
}

/// `POST /protocols/{slug}`.
#[derive(Deserialize)]
struct ToggleForm {
    #[serde(default)]
    enabled: String,
    #[serde(default)]
    csrf: String,
}

async fn toggle(
    State(site): State<Arc<SiteState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Form(form): Form<ToggleForm>,
) -> Response {
    let session = match site.require_session(&headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if !csrf_matches(&session, &form.csrf) {
        return csrf_refused(&session);
    }
    let user = Some(UserView::from(&session));
    let Some(registration) = site.catalog.get(&slug) else {
        return error_page(
            StatusCode::NOT_FOUND,
            user,
            "Unknown protocol",
            "That protocol is not offered here.",
        );
    };
    let select = match form.enabled.as_str() {
        "on" => Select::On {
            revision: registration.revision(),
        },
        "off" => Select::Off,
        _ => {
            return error_page(
                StatusCode::BAD_REQUEST,
                user,
                "Invalid request",
                "Choose on or off.",
            );
        }
    };
    if let Err(err) = site.store.set_selection(&session.sub, &slug, select).await {
        warn!(code = %err.code(), "changing a selection failed: {err}");
        return unavailable_page(user);
    }
    info!(registration = %slug, enabled = %form.enabled, "selection changed on the site");
    see_other("/protocols").into_response()
}

async fn connect(State(site): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    let session = match site.require_session(&headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let rows = match site.rows(&session.sub).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    render(
        StatusCode::OK,
        &ConnectPage {
            user: Some(UserView::from(&session)),
            mcp_url: site.mcp_url.clone(),
            enabled: rows
                .into_iter()
                .filter(|r| r.status == RowStatus::Enabled)
                .collect(),
            refresh_sentence: REFRESH_SENTENCE,
            never_signs_sentence: NEVER_SIGNS_SENTENCE,
        },
    )
}

async fn css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        CSS,
    )
        .into_response()
}
