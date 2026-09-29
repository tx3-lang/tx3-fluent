//! `fluent serve --http` with `oidc` authentication and a `[store]`, in
//! process: each user sees and calls only the registrations they selected, at
//! the revision they selected; revocation and restarts.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use fluent_core::catalog::{self, ToolDescriptor};
use fluent_core::registration::load_dir;
use fluent_core::{Catalog, Engine};
use fluent_server::mcp::FluentHandler;
use fluent_server::store::{Select, SelectionStatus, Store, UserScopes};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STRIKE: &str = "strike_staking_mainnet";
const TRANSFER: &str = "transfer_preprod";

/// A copy of the valid fixture bundles and a store file, surviving server
/// restarts.
struct Site {
    dir: tempfile::TempDir,
    issuer: MockServer,
}

impl Site {
    async fn new() -> Site {
        let dir = tempfile::tempdir().expect("temp dir");
        copy_dir(
            &fixtures().join("registrations/valid"),
            &dir.path().join("registrations"),
        );
        let (a, _) = keys();
        let issuer = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[a])))
            .mount(&issuer)
            .await;
        Site { dir, issuer }
    }

    fn registrations(&self) -> PathBuf {
        self.dir.path().join("registrations")
    }

    fn store_path(&self) -> PathBuf {
        self.dir.path().join("fluent.sqlite")
    }

    /// Starts a server over the bundles as they are now, the way
    /// `fluent serve --http` does with a `[store]`.
    async fn start(&self) -> Hosted {
        let text = format!(
            "[server]\nlisten = \"127.0.0.1:0\"\npublic_url = \"{PUBLIC_URL}\"\n\n\
             [registrations]\ndir = {}\n\n\
             [networks.preprod]\ntrp_url = \"http://127.0.0.1:9\"\n\n\
             [store]\nsqlite_path = {}\n\n\
             [auth]\n{}\n",
            toml::Value::String(self.registrations().display().to_string()),
            toml::Value::String(self.store_path().display().to_string()),
            oidc_auth(&format!("{}/jwks.json", self.issuer.uri())),
        );
        let config = load_config(&text, &[]);
        let registrations = load_dir(&config.registrations.dir)
            .await
            .expect("load registrations");
        assert!(registrations.rejected.is_empty());
        let catalog = Arc::new(registrations.catalog);
        let engine = Arc::new(Engine::new(&config, &catalog));
        let store = Store::open(&self.store_path()).await.expect("open store");
        let scoping = Arc::new(UserScopes::new(store.clone(), Arc::clone(&catalog)));
        let handler = FluentHandler::new(Arc::clone(&catalog), engine, scoping).expect("handler");
        let server = Running::start_with(&config, handler).await;
        Hosted {
            server,
            store,
            catalog,
        }
    }
}

/// A running server and the store and catalog behind it.
struct Hosted {
    server: Running,
    store: Store,
    catalog: Arc<Catalog>,
}

impl Hosted {
    /// Selects registration `slug` at its current revision for `sub`.
    async fn select(&self, sub: &str, slug: &str) {
        let revision = self.catalog.get(slug).expect("registration").revision();
        self.store
            .set_selection(sub, slug, Select::On { revision })
            .await
            .expect("select");
    }

    /// Opens a session as `sub`; its token and session id.
    async fn session(&self, sub: &str) -> (String, String) {
        let token = keys().0.sign(&claims(sub));
        let (status, session) = self.server.initialize(Some(&token)).await;
        assert_eq!(status, 200, "{sub}");
        (token, session.expect("a session id"))
    }

    /// The tool names `tools/list` returns in `session`.
    async fn listed(&self, (token, session): &(String, String)) -> BTreeSet<String> {
        let reply = rpc_reply(
            self.server
                .post_mcp(&list_tools(), Some(token), Some(session))
                .await,
        )
        .await;
        reply["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("a tool list: {reply}"))
            .iter()
            .map(|tool| tool["name"].as_str().expect("a tool name").to_string())
            .collect()
    }

    /// The tool result of calling `name` with `args` in `session`.
    async fn call(&self, (token, session): &(String, String), name: &str, args: Value) -> Value {
        let reply = rpc_reply(
            self.server
                .post_mcp(&call_tool(name, args), Some(token), Some(session))
                .await,
        )
        .await;
        reply["result"].clone()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Every tool of the catalog for registration `slug`.
fn tools_of(catalog: &Catalog, slug: &str) -> BTreeSet<String> {
    let tools: Vec<ToolDescriptor> = catalog::all_tools(catalog).expect("tools");
    let names: BTreeSet<String> = tools
        .into_iter()
        .filter(|t| t.registration_slug.as_deref() == Some(slug))
        .map(|t| t.name)
        .collect();
    assert!(!names.is_empty(), "{slug} has tools");
    names
}

fn fixed_tools() -> BTreeSet<String> {
    ["fluent_get_skill", "fluent_inspect_address"]
        .map(String::from)
        .into()
}

fn with_fixed(tools: &BTreeSet<String>) -> BTreeSet<String> {
    tools.union(&fixed_tools()).cloned().collect()
}

/// The `error.code` of a tool result, asserting it is an error.
fn error_code(result: &Value) -> String {
    assert_eq!(result["isError"], json!(true), "{result}");
    let text = result["content"][0]["text"].as_str().expect("error text");
    let error: Value = serde_json::from_str(text).expect("error JSON");
    error["error"]["code"]
        .as_str()
        .expect("an error code")
        .to_string()
}

/// The first transaction tool of `slug`, called with no arguments.
fn any_tool(catalog: &Catalog, slug: &str) -> String {
    tools_of(catalog, slug).into_iter().next().expect("a tool")
}

#[tokio::test]
async fn two_users_see_disjoint_tool_lists() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", STRIKE).await;
    hosted.select("bob", TRANSFER).await;

    let alice = hosted.listed(&hosted.session("alice").await).await;
    let bob = hosted.listed(&hosted.session("bob").await).await;
    let carol = hosted.listed(&hosted.session("carol").await).await;
    println!("alice (selected {STRIKE}) sees: {alice:?}");
    println!("bob (selected {TRANSFER}) sees: {bob:?}");
    println!("carol (selected nothing) sees: {carol:?}");

    assert_eq!(alice, with_fixed(&tools_of(&hosted.catalog, STRIKE)));
    assert_eq!(bob, with_fixed(&tools_of(&hosted.catalog, TRANSFER)));
    assert_eq!(carol, fixed_tools());
    let fixed = fixed_tools();
    let alice_only: BTreeSet<_> = alice.difference(&fixed).collect();
    let bob_only: BTreeSet<_> = bob.difference(&fixed).collect();
    assert!(alice_only.is_disjoint(&bob_only));
}

#[tokio::test]
async fn calling_an_unselected_tool_is_registration_unavailable() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", STRIKE).await;
    let alice = hosted.session("alice").await;

    let result = hosted
        .call(&alice, &any_tool(&hosted.catalog, TRANSFER), json!({}))
        .await;
    assert_eq!(error_code(&result), "registration_unavailable");

    // A selected tool gets past the scope check.
    let result = hosted
        .call(&alice, &any_tool(&hosted.catalog, STRIKE), json!({}))
        .await;
    assert_ne!(error_code(&result), "registration_unavailable", "{result}");

    // Nor is an unselected registration's skill served.
    let result = hosted
        .call(&alice, "fluent_get_skill", json!({"protocol": TRANSFER}))
        .await;
    assert_eq!(result["isError"], json!(true), "{result}");
    let result = hosted
        .call(&alice, "fluent_get_skill", json!({"protocol": STRIKE}))
        .await;
    assert_ne!(result["isError"], json!(true), "{result}");
}

#[tokio::test]
async fn a_revision_bump_hides_the_registration_until_selected_again() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", STRIKE).await;
    hosted.select("alice", TRANSFER).await;
    let before = hosted.catalog.get(STRIKE).unwrap().revision().to_string();
    drop(hosted);

    // A new skill text is a new revision.
    let skill = site.registrations().join(STRIKE).join("SKILL.md");
    let mut text = std::fs::read_to_string(&skill).expect("read skill");
    text.push_str("\nRevised.\n");
    std::fs::write(&skill, text).expect("write skill");

    let hosted = site.start().await;
    assert_ne!(hosted.catalog.get(STRIKE).unwrap().revision(), before);
    let alice = hosted.session("alice").await;
    assert_eq!(
        hosted.listed(&alice).await,
        with_fixed(&tools_of(&hosted.catalog, TRANSFER))
    );
    let result = hosted
        .call(&alice, &any_tool(&hosted.catalog, STRIKE), json!({}))
        .await;
    assert_eq!(error_code(&result), "registration_unavailable");

    let statuses: Vec<(String, SelectionStatus)> = hosted
        .store
        .list_selections("alice")
        .await
        .unwrap()
        .into_iter()
        .map(|s| {
            let status = s.status(&hosted.catalog);
            (s.slug, status)
        })
        .collect();
    assert_eq!(
        statuses,
        [
            (STRIKE.to_string(), SelectionStatus::UpdateRequired),
            (TRANSFER.to_string(), SelectionStatus::Active),
        ]
    );

    // Selecting it again at the new revision shows it in the same session.
    hosted.select("alice", STRIKE).await;
    assert_eq!(
        hosted.listed(&alice).await,
        with_fixed(
            &tools_of(&hosted.catalog, STRIKE)
                .union(&tools_of(&hosted.catalog, TRANSFER))
                .cloned()
                .collect()
        )
    );
}

#[tokio::test]
async fn a_removed_registration_disappears() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", STRIKE).await;
    let removed = any_tool(&hosted.catalog, STRIKE);
    drop(hosted);

    std::fs::remove_dir_all(site.registrations().join(STRIKE)).expect("remove bundle");
    let hosted = site.start().await;
    let alice = hosted.session("alice").await;
    assert_eq!(hosted.listed(&alice).await, fixed_tools());
    let selection = &hosted.store.list_selections("alice").await.unwrap()[0];
    assert_eq!(selection.status(&hosted.catalog), SelectionStatus::Removed);
    // No registration defines the tool any more: a protocol error.
    let reply = rpc_reply(
        hosted
            .server
            .post_mcp(
                &call_tool(&removed, json!({})),
                Some(&alice.0),
                Some(&alice.1),
            )
            .await,
    )
    .await;
    assert!(reply.get("error").is_some(), "{reply}");
}

#[tokio::test]
async fn revocation_empties_the_list_and_rejects_calls() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", STRIKE).await;
    let open = hosted.session("alice").await;
    assert_eq!(
        hosted.listed(&open).await,
        with_fixed(&tools_of(&hosted.catalog, STRIKE))
    );

    // Revoked through another handle on the file, as `fluent admin revoke`
    // does: the open session loses its tools on its next request.
    let admin = Store::open(&site.store_path()).await.expect("open store");
    admin.revoke("alice").await.expect("revoke");

    assert!(hosted.listed(&open).await.is_empty());
    for (name, args) in [
        (any_tool(&hosted.catalog, STRIKE), json!({})),
        (
            "fluent_inspect_address".to_string(),
            json!({"address": "x"}),
        ),
    ] {
        let result = hosted.call(&open, &name, args).await;
        assert_eq!(error_code(&result), "unauthorized", "{name}");
    }
    let fresh = hosted.session("alice").await;
    assert!(hosted.listed(&fresh).await.is_empty());

    // Others are unaffected.
    hosted.select("bob", STRIKE).await;
    let bob = hosted.session("bob").await;
    assert_eq!(
        hosted.listed(&bob).await,
        with_fixed(&tools_of(&hosted.catalog, STRIKE))
    );
}

#[tokio::test]
async fn selections_survive_a_restart() {
    let site = Site::new().await;
    let hosted = site.start().await;
    hosted.select("alice", TRANSFER).await;
    let expected = with_fixed(&tools_of(&hosted.catalog, TRANSFER));
    drop(hosted);

    let hosted = site.start().await;
    let alice = hosted.session("alice").await;
    assert_eq!(hosted.listed(&alice).await, expected);
    let user = hosted.store.user("alice").await.unwrap().expect("a user");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
}

#[tokio::test]
async fn a_selection_change_notifies_the_users_open_sessions() {
    let site = Site::new().await;
    let hosted = site.start().await;
    let (token, session) = hosted.session("alice").await;

    let stream = hosted
        .server
        .client
        .get(hosted.server.url("/mcp"))
        .header("accept", "text/event-stream")
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .send()
        .await
        .expect("GET /mcp");
    assert_eq!(stream.status(), 200);

    // Someone else's change is not announced to alice.
    let mut stream = stream;
    let mut seen = String::new();
    hosted.select("bob", STRIKE).await;
    let quiet = tokio::time::Instant::now() + Duration::from_millis(500);
    while let Ok(chunk) = tokio::time::timeout_at(quiet, stream.chunk()).await {
        let chunk = chunk.expect("stream chunk").expect("stream open");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(!seen.contains("notifications/tools/list_changed"), "{seen}");

    hosted.select("alice", STRIKE).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains("notifications/tools/list_changed") {
        let chunk = tokio::time::timeout_at(deadline, stream.chunk())
            .await
            .expect("a notification before the deadline")
            .expect("stream chunk")
            .expect("stream open");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}
