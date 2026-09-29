//! The companion site, in process: `fluent serve --http` with `oidc`
//! authentication, a `[store]` and `[site]`. Sign-in runs against a
//! `wiremock` authorization server; other tests present a session cookie
//! signed with the configured secret.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use fluent_core::registration::load_dir;
use fluent_core::{Catalog, Engine};
use fluent_server::mcp::FluentHandler;
use fluent_server::site::session::{Cookies, SESSION_COOKIE, Session};
use fluent_server::site::{BRAND_LINE, NEVER_SIGNS_SENTENCE, REFRESH_SENTENCE, Site};
use fluent_server::store::{Select, Store, UserScopes};
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use reqwest::{Response, StatusCode};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STRIKE: &str = "strike_staking_mainnet";
const TRANSFER: &str = "transfer_preprod";
const SESSION_SECRET: &str = "a session secret of at least thirty-two bytes";
const CLIENT_ID: &str = "fluent-site";
const CLIENT_SECRET: &str = "client-secret";
const CSRF: &str = "csrf-token-of-this-session";

/// A running server with the site, its store and catalog, and the issuer it
/// signs users in with.
struct Hosted {
    server: Running,
    /// Never follows redirects, so tests see them.
    client: reqwest::Client,
    store: Store,
    catalog: Arc<Catalog>,
    issuer: MockServer,
    _dir: tempfile::TempDir,
}

impl Hosted {
    /// Starts a server over the valid fixture bundles with `[site].enabled`
    /// set to `site`, the way `fluent serve --http` does.
    async fn start(site: bool) -> Hosted {
        let dir = tempfile::tempdir().expect("temp dir");
        let issuer = MockServer::start().await;
        let (a, _) = keys();
        Mock::given(method("GET"))
            .and(path("/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[a])))
            .mount(&issuer)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": format!("{}/", issuer.uri()),
                "authorization_endpoint": format!("{}/authorize", issuer.uri()),
                "token_endpoint": format!("{}/oauth/token", issuer.uri()),
                "userinfo_endpoint": format!("{}/userinfo", issuer.uri()),
                "jwks_uri": format!("{}/jwks.json", issuer.uri()),
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
            })))
            .mount(&issuer)
            .await;

        let text = format!(
            "[server]\nlisten = \"127.0.0.1:0\"\npublic_url = \"{PUBLIC_URL}\"\n\n\
             [registrations]\ndir = {}\n\n\
             [networks.preprod]\ntrp_url = \"http://127.0.0.1:9\"\n\n\
             [store]\nsqlite_path = {}\n\n\
             [auth]\nmode = \"oidc\"\nissuer = \"{}/\"\njwks_url = \"{}/jwks.json\"\n\
             audience = \"{AUDIENCE}\"\n\n\
             [site]\nenabled = {site}\nsession_secret_env = \"FLUENT_SESSION_SECRET\"\n\
             oidc_client_id_env = \"FLUENT_OIDC_CLIENT_ID\"\n\
             oidc_client_secret_env = \"FLUENT_OIDC_CLIENT_SECRET\"\n\
             redirect_url = \"{PUBLIC_URL}/auth/callback\"\n",
            toml::Value::String(fixtures().join("registrations/valid").display().to_string()),
            toml::Value::String(dir.path().join("fluent.sqlite").display().to_string()),
            issuer.uri(),
            issuer.uri(),
        );
        let config = load_config(
            &text,
            &[
                ("FLUENT_SESSION_SECRET", SESSION_SECRET),
                ("FLUENT_OIDC_CLIENT_ID", CLIENT_ID),
                ("FLUENT_OIDC_CLIENT_SECRET", CLIENT_SECRET),
            ],
        );
        let registrations = load_dir(&config.registrations.dir)
            .await
            .expect("load registrations");
        assert!(registrations.rejected.is_empty());
        let catalog = Arc::new(registrations.catalog);
        let engine = Arc::new(Engine::new(&config, &catalog));
        let store = Store::open(&dir.path().join("fluent.sqlite"))
            .await
            .expect("open store");
        let site = Site::new(&config, store.clone(), Arc::clone(&catalog)).expect("site");
        assert_eq!(site.is_some(), config.site.enabled);
        let scoping = Arc::new(UserScopes::new(store.clone(), Arc::clone(&catalog)));
        let handler = FluentHandler::new(Arc::clone(&catalog), engine, scoping).expect("handler");
        let server = Running::start_with_site(&config, handler, site).await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP client");
        Hosted {
            server,
            client,
            store,
            catalog,
            issuer,
            _dir: dir,
        }
    }

    /// The `Cookie` header of a signed-in session of `sub`, as a sign-in
    /// would have set it.
    fn session_cookie(&self, sub: &str) -> String {
        let cookies = Cookies::new(SESSION_SECRET, true).expect("cookies");
        cookies.header_for(
            SESSION_COOKIE,
            &Session {
                sub: sub.to_string(),
                display: format!("{sub}@example.com"),
                csrf: CSRF.to_string(),
                expires_at: i64::try_from(now()).expect("a sane clock") + 3600,
            },
        )
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> Response {
        let mut request = self.client.get(self.server.url(path));
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        request.send().await.expect("GET")
    }

    async fn post(&self, path: &str, cookie: Option<&str>, form: &[(&str, &str)]) -> Response {
        let mut request = self.client.post(self.server.url(path)).form(form);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        request.send().await.expect("POST")
    }

    /// The revision `sub` selected `slug` at, if they did.
    async fn selected(&self, sub: &str, slug: &str) -> Option<String> {
        self.store
            .list_selections(sub)
            .await
            .expect("selections")
            .into_iter()
            .find(|s| s.slug == slug)
            .map(|s| s.revision)
    }

    fn revision(&self, slug: &str) -> String {
        self.catalog
            .get(slug)
            .expect("registration")
            .revision()
            .to_string()
    }
}

fn location(response: &Response) -> &str {
    response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_else(|| panic!("a Location header: {response:?}"))
}

/// The `Set-Cookie` header setting cookie `name`.
fn set_cookie<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
}

/// The `name=value` part of a `Set-Cookie` header.
fn cookie_pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().expect("a cookie pair")
}

/// The value of query parameter `name` in `url`.
fn query_param(url: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .expect("a URL")
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

#[tokio::test]
async fn site_routes_are_absent_when_the_site_is_disabled() {
    let hosted = Hosted::start(false).await;
    let cookie = hosted.session_cookie("alice");
    for path in [
        "/",
        "/auth/login",
        "/auth/callback",
        "/protocols",
        "/connect",
        "/site.css",
    ] {
        let response = hosted.get(path, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {path}");
    }
    for path in ["/auth/logout", "/protocols/transfer_preprod"] {
        let response = hosted
            .post(path, Some(&cookie), &[("csrf", CSRF), ("enabled", "on")])
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "POST {path}");
    }
    // The MCP routes are unaffected.
    assert_eq!(hosted.get("/healthz", None).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_landing_page_offers_sign_in() {
    let hosted = Hosted::start(true).await;
    let response = hosted.get("/", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let csp = response
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .expect("a CSP")
        .to_string();
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    let body = response.text().await.expect("body");
    assert!(body.contains(BRAND_LINE), "{body}");
    assert!(body.contains("href=\"/auth/login\""), "{body}");
    assert!(!body.contains("Signed in as"), "{body}");

    let css = hosted.get("/site.css", None).await;
    assert_eq!(css.status(), StatusCode::OK);
    assert!(
        css.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
}

#[tokio::test]
async fn signed_out_requests_redirect_to_the_landing_page() {
    let hosted = Hosted::start(true).await;
    for path in ["/protocols", "/connect"] {
        let response = hosted.get(path, None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "GET {path}");
        assert_eq!(location(&response), "/", "GET {path}");
    }
    let response = hosted
        .post(
            &format!("/protocols/{TRANSFER}"),
            None,
            &[("csrf", CSRF), ("enabled", "on")],
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/");

    // A cookie signed with another secret is no session.
    let forged = Cookies::new("another secret, as long as the real one", true)
        .expect("cookies")
        .header_for(
            SESSION_COOKIE,
            &Session {
                sub: "alice".into(),
                display: "alice".into(),
                csrf: CSRF.into(),
                expires_at: i64::MAX,
            },
        );
    let response = hosted.get("/protocols", Some(&forged)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(hosted.store.user("alice").await.unwrap().is_none());
}

#[tokio::test]
async fn the_protocols_page_lists_every_registration_with_its_status() {
    let hosted = Hosted::start(true).await;
    // Selected at an earlier revision: update required.
    hosted
        .store
        .set_selection("alice", STRIKE, Select::On { revision: "old" })
        .await
        .unwrap();

    let response = hosted
        .get("/protocols", Some(&hosted.session_cookie("alice")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.text().await.expect("body");
    assert!(
        body.contains("Signed in as <strong>alice@example.com</strong>"),
        "{body}"
    );
    for (slug, network) in [(STRIKE, "mainnet"), (TRANSFER, "preprod")] {
        let registration = hosted.catalog.get(slug).unwrap();
        let protocol = &registration.manifest().protocol;
        assert!(
            body.contains(&format!("{}/{}", protocol.scope, protocol.name)),
            "{slug}"
        );
        assert!(body.contains(&protocol.version), "{slug}");
        assert!(body.contains(registration.revision()), "{slug}");
        assert!(
            body.contains(&format!("action=\"/protocols/{slug}\"")),
            "{slug}"
        );
        assert!(
            body.contains(&format!("network-{network}\">{network}</span>")),
            "{slug}"
        );
    }
    assert!(body.contains(">update required</span>"), "{body}");
    assert!(body.contains(">disabled</span>"), "{body}");
    assert!(body.contains(&format!("value=\"{CSRF}\"")), "{body}");
}

#[tokio::test]
async fn toggling_selects_at_the_current_revision_and_notifies_sessions() {
    let hosted = Hosted::start(true).await;
    let cookie = hosted.session_cookie("alice");

    // alice has an MCP session open; its event stream carries notifications.
    let mut claims = claims("alice");
    claims["iss"] = json!(format!("{}/", hosted.issuer.uri()));
    let token = keys().0.sign(&claims);
    let (status, session) = hosted.server.initialize(Some(&token)).await;
    assert_eq!(status, 200);
    let mut stream = hosted
        .server
        .client
        .get(hosted.server.url("/mcp"))
        .header("accept", "text/event-stream")
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-session-id", session.expect("a session id"))
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .send()
        .await
        .expect("GET /mcp");
    assert_eq!(stream.status(), 200);

    let path = format!("/protocols/{TRANSFER}");
    let response = hosted
        .post(&path, Some(&cookie), &[("enabled", "on"), ("csrf", CSRF)])
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/protocols");
    assert_eq!(
        hosted.selected("alice", TRANSFER).await,
        Some(hosted.revision(TRANSFER))
    );

    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains("notifications/tools/list_changed") {
        let chunk = tokio::time::timeout_at(deadline, stream.chunk())
            .await
            .expect("a notification before the deadline")
            .expect("stream chunk")
            .expect("stream open");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }

    let body = hosted
        .get("/protocols", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(body.contains(">enabled</span>"), "{body}");

    let response = hosted
        .post(&path, Some(&cookie), &[("enabled", "off"), ("csrf", CSRF)])
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(hosted.selected("alice", TRANSFER).await, None);
}

#[tokio::test]
async fn updating_a_stale_selection_records_the_current_revision() {
    let hosted = Hosted::start(true).await;
    hosted
        .store
        .set_selection("alice", STRIKE, Select::On { revision: "old" })
        .await
        .unwrap();
    let response = hosted
        .post(
            &format!("/protocols/{STRIKE}"),
            Some(&hosted.session_cookie("alice")),
            &[("enabled", "on"), ("csrf", CSRF)],
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        hosted.selected("alice", STRIKE).await,
        Some(hosted.revision(STRIKE))
    );
}

#[tokio::test]
async fn forms_without_the_sessions_csrf_token_are_refused() {
    let hosted = Hosted::start(true).await;
    let cookie = hosted.session_cookie("alice");
    let path = format!("/protocols/{TRANSFER}");
    for form in [
        vec![("enabled", "on")],
        vec![("enabled", "on"), ("csrf", "")],
        vec![("enabled", "on"), ("csrf", "another-token")],
    ] {
        let response = hosted.post(&path, Some(&cookie), &form).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{form:?}");
    }
    assert_eq!(hosted.selected("alice", TRANSFER).await, None);

    let response = hosted
        .post("/auth/logout", Some(&cookie), &[("csrf", "another-token")])
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
}

#[tokio::test]
async fn unknown_protocols_and_values_are_rejected() {
    let hosted = Hosted::start(true).await;
    let cookie = hosted.session_cookie("alice");
    let response = hosted
        .post(
            "/protocols/no_such_protocol",
            Some(&cookie),
            &[("enabled", "on"), ("csrf", CSRF)],
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = hosted
        .post(
            &format!("/protocols/{TRANSFER}"),
            Some(&cookie),
            &[("enabled", "maybe"), ("csrf", CSRF)],
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        hosted
            .store
            .list_selections("alice")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_connect_page_gives_the_server_url_and_chatgpt_steps() {
    let hosted = Hosted::start(true).await;
    hosted
        .store
        .set_selection(
            "alice",
            TRANSFER,
            Select::On {
                revision: &hosted.revision(TRANSFER),
            },
        )
        .await
        .unwrap();
    let response = hosted
        .get("/connect", Some(&hosted.session_cookie("alice")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("body");
    assert!(body.contains(&format!("{PUBLIC_URL}/mcp")), "{body}");
    assert!(body.contains("<ol class=\"steps\">"), "{body}");
    assert!(body.contains("developer mode"), "{body}");
    assert!(body.contains(REFRESH_SENTENCE), "{body}");
    assert!(body.contains(NEVER_SIGNS_SENTENCE), "{body}");
    assert!(body.contains("alice@example.com"), "{body}");
    assert!(
        body.contains("network-preprod\">preprod</span>"),
        "the enabled protocol and its network: {body}"
    );
    assert!(!body.contains("mainnet"), "only enabled protocols: {body}");
}

#[tokio::test]
async fn sign_in_runs_the_authorization_code_flow_with_pkce() {
    let hosted = Hosted::start(true).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&hosted.issuer)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sub": "auth0|alice",
            "email": "alice@example.com",
            "name": "Alice",
        })))
        .expect(1)
        .mount(&hosted.issuer)
        .await;

    // 1. The authorize redirect.
    let login = hosted.get("/auth/login", None).await;
    assert_eq!(login.status(), StatusCode::SEE_OTHER);
    let authorize = location(&login).to_string();
    assert!(
        authorize.starts_with(&format!("{}/authorize?", hosted.issuer.uri())),
        "{authorize}"
    );
    let param = |name| query_param(&authorize, name);
    assert_eq!(param("response_type").as_deref(), Some("code"));
    assert_eq!(param("client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(
        param("redirect_uri"),
        Some(format!("{PUBLIC_URL}/auth/callback"))
    );
    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    assert!(param("code_challenge").is_some_and(|c| c.len() == 43));
    let scope = param("scope").expect("a scope");
    assert!(scope.split(' ').any(|s| s == "openid"), "{scope}");
    let state = param("state").expect("a state");
    let pending = set_cookie(&login, "fluent_login").expect("the sign-in cookie");
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/auth", "Secure"] {
        assert!(pending.contains(attribute), "{attribute}: {pending}");
    }
    let pending = cookie_pair(pending).to_string();

    // 2. The callback exchanges the code and reads userinfo.
    let callback = hosted
        .get(
            &format!("/auth/callback?code=the-code&state={state}"),
            Some(&pending),
        )
        .await;
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&callback), "/protocols");
    let session = set_cookie(&callback, SESSION_COOKIE).expect("the session cookie");
    for attribute in [
        "HttpOnly",
        "SameSite=Lax",
        "Path=/",
        "Secure",
        "Max-Age=604800",
    ] {
        assert!(session.contains(attribute), "{attribute}: {session}");
    }
    let cleared = set_cookie(&callback, "fluent_login").expect("the sign-in cookie cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let session = cookie_pair(session).to_string();

    // The token request sent the redirect URI and authenticated the client.
    let requests = hosted.issuer.received_requests().await.expect("requests");
    let token_request = requests
        .iter()
        .find(|r| r.url.path() == "/oauth/token")
        .expect("a token request");
    let body = String::from_utf8_lossy(&token_request.body);
    assert!(
        body.contains(&format!(
            "redirect_uri={}",
            "https%3A%2F%2Ffluent.test%2Fauth%2Fcallback"
        )),
        "{body}"
    );
    // The issuer offers `client_secret_post`, so the client authenticates in
    // the body.
    assert!(body.contains(&format!("client_id={CLIENT_ID}")), "{body}");
    assert!(
        body.contains(&format!("client_secret={CLIENT_SECRET}")),
        "{body}"
    );
    assert!(token_request.headers.get("authorization").is_none());

    // 3. The session is the userinfo subject.
    let user = hosted
        .store
        .user("auth0|alice")
        .await
        .unwrap()
        .expect("the store user");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    let page = hosted.get("/protocols", Some(&session)).await;
    assert_eq!(page.status(), StatusCode::OK);
    let body = page.text().await.unwrap();
    assert!(
        body.contains("Signed in as <strong>alice@example.com</strong>"),
        "{body}"
    );

    // 4. Signing out clears the session.
    let csrf = body
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a CSRF token")
        .to_string();
    let logout = hosted
        .post("/auth/logout", Some(&session), &[("csrf", &csrf)])
        .await;
    assert_eq!(logout.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&logout), "/");
    let cleared = set_cookie(&logout, SESSION_COOKIE).expect("the session cookie cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
}

#[tokio::test]
async fn callbacks_without_a_matching_sign_in_are_refused() {
    let hosted = Hosted::start(true).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&hosted.issuer)
        .await;

    // No sign-in cookie.
    let response = hosted.get("/auth/callback?code=c&state=s", None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Another sign-in's state.
    let login = hosted.get("/auth/login", None).await;
    let pending = cookie_pair(set_cookie(&login, "fluent_login").unwrap()).to_string();
    let response = hosted
        .get("/auth/callback?code=c&state=forged", Some(&pending))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());

    // The provider's refusal.
    let response = hosted
        .get("/auth/callback?error=access_denied", Some(&pending))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("refused the sign-in (access_denied)")
    );
}

#[tokio::test]
async fn revoked_users_are_refused() {
    let hosted = Hosted::start(true).await;
    hosted.store.revoke("alice").await.unwrap();
    let response = hosted
        .get("/protocols", Some(&hosted.session_cookie("alice")))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let cleared = set_cookie(&response, SESSION_COOKIE).expect("the session cookie cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let response = hosted
        .post(
            &format!("/protocols/{TRANSFER}"),
            Some(&hosted.session_cookie("alice")),
            &[("enabled", "on"), ("csrf", CSRF)],
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(hosted.selected("alice", TRANSFER).await, None);
}
