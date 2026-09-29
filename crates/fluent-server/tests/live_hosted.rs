//! Prepares a transfer in hosted mode against a live preprod TRP endpoint:
//! the hosted bundles in `deploy/hosted/registrations/`, fetched from their
//! pinned registry artifacts, served over HTTP in `oidc` mode with a
//! `[store]`, for a test user who selected `transfer_preprod`. The prepared
//! CBOR is then checked independently with `xtask::verify`.
//!
//! Skips green unless `FLUENT_TRP_URL_PREPROD` is set. When it is, the test
//! also needs `TEST_PARTY_A_ADDRESS` (a funded preprod sender) and
//! `TEST_PARTY_B_ADDRESS` (the receiver), and sends
//! `FLUENT_TRP_API_KEY_PREPROD` as the API key when it is set; a set URL with
//! a missing party address fails rather than skipping. The issuer is a local
//! JWKS, so no identity-provider secret is needed. Nothing is signed or
//! submitted.
//!
//! ```sh
//! FLUENT_TRP_URL_PREPROD=https://cardano-preprod.trp-m1.demeter.run \
//! FLUENT_TRP_API_KEY_PREPROD=… TEST_PARTY_A_ADDRESS=addr_test1… \
//! TEST_PARTY_B_ADDRESS=addr_test1… \
//! cargo test -p fluent-server --test live_hosted -- --nocapture
//! ```

mod common;

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use fluent_core::Engine;
use fluent_core::registration::{Network, load_dir};
use fluent_server::mcp::FluentHandler;
use fluent_server::store::{Select, Store, UserScopes};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xtask::verify::{Expectations, Verdict, verify};

const API_KEY_VAR: &str = "FLUENT_TRP_API_KEY_PREPROD";
const TEST_USER: &str = "live|fluent-hosted-ci";
const QUANTITY: u64 = 2_000_000;

fn set(var: &str) -> Option<String> {
    env::var(var).ok().filter(|value| !value.is_empty())
}

fn required(var: &str) -> String {
    set(var).unwrap_or_else(|| {
        panic!(
            "FLUENT_TRP_URL_PREPROD is set but {var} is not: refusing a half-configured live run"
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn prepares_a_hosted_transfer_for_a_selected_user() {
    let Some(trp_url) = set("FLUENT_TRP_URL_PREPROD") else {
        eprintln!("skipping: FLUENT_TRP_URL_PREPROD is not set");
        return;
    };
    let sender = required("TEST_PARTY_A_ADDRESS");
    let receiver = required("TEST_PARTY_B_ADDRESS");

    let issuer = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[&keys().0])))
        .mount(&issuer)
        .await;
    let dir = tempfile::tempdir().expect("temp dir");
    let store_path = dir.path().join("fluent.sqlite");
    // A copy, because the loader caches registry artifacts beside the
    // bundles.
    let hosted = dir.path().join("registrations");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/hosted/registrations"),
        &hosted,
    );
    let text = format!(
        "[server]\nlisten = \"127.0.0.1:0\"\npublic_url = \"{PUBLIC_URL}\"\n\n\
         [registrations]\ndir = {}\n\n\
         [networks.preprod]\ntrp_url = {}\ntrp_api_key_env = \"{API_KEY_VAR}\"\n\n\
         [limits]\nresolver_timeout_secs = 60\nglobal_cutoff_secs = 60\n\n\
         [store]\nsqlite_path = {}\n\n\
         [auth]\n{}\n",
        toml::Value::String(hosted.display().to_string()),
        toml::Value::String(trp_url),
        toml::Value::String(store_path.display().to_string()),
        oidc_auth(&format!("{}/jwks.json", issuer.uri())),
    );
    let key: Vec<(&str, String)> = set(API_KEY_VAR)
        .map(|key| (API_KEY_VAR, key))
        .into_iter()
        .collect();
    let key: Vec<(&str, &str)> = key.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let config = load_config(&text, &key);

    // The way `fluent serve --http` builds a hosted server.
    let registrations = load_dir(&config.registrations.dir)
        .await
        .expect("load the hosted bundles");
    assert!(
        registrations.rejected.is_empty(),
        "rejected: {:?}",
        registrations
            .rejected
            .iter()
            .map(|r| r.error.message())
            .collect::<Vec<_>>()
    );
    let catalog = Arc::new(registrations.catalog);
    let engine = Arc::new(Engine::new(&config, &catalog));
    let store = Store::open(&store_path).await.expect("open the store");
    let revision = catalog
        .get("transfer_preprod")
        .expect("the hosted transfer")
        .revision()
        .to_string();
    store
        .set_selection(
            TEST_USER,
            "transfer_preprod",
            Select::On {
                revision: &revision,
            },
        )
        .await
        .expect("select the transfer");
    let scoping = Arc::new(UserScopes::new(store, Arc::clone(&catalog)));
    let handler = FluentHandler::new(catalog, engine, scoping).expect("handler");
    let server = Running::start_with(&config, handler).await;

    let token = keys().0.sign(&claims(TEST_USER));
    let (status, session) = server.initialize(Some(&token)).await;
    assert_eq!(status, 200);
    let session = session.expect("a session id");
    let listed = rpc_reply(
        server
            .post_mcp(&list_tools(), Some(&token), Some(&session))
            .await,
    )
    .await;
    assert!(
        listed.to_string().contains("\"transfer_preprod_transfer\""),
        "{listed}"
    );

    let prepared = call(
        &server,
        &token,
        &session,
        "transfer_preprod_transfer",
        json!({ "quantity": QUANTITY, "sender": sender, "receiver": receiver }),
    )
    .await;
    assert_eq!(prepared["status"], "prepared_unsigned", "{prepared}");
    assert_eq!(prepared["network"], "preprod");
    assert_eq!(prepared["protocol"]["scope"], "open-tx3");
    assert_eq!(
        prepared["protocol"]["registration_slug"],
        "transfer_preprod"
    );

    let cbor = prepared["unsigned_tx_cbor_hex"].as_str().expect("the CBOR");
    let expected = Expectations {
        outputs: vec![
            format!("{receiver}={QUANTITY}")
                .parse()
                .expect("expectation"),
        ],
        network: Network::Preprod,
        signers: Vec::new(),
    };
    let verification = verify(cbor, &expected).expect("decode the CBOR");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(verification.tx_hash, prepared["tx_hash"]);
    eprintln!(
        "prepared {} for a selected hosted user; the independent decode check matched",
        verification.tx_hash
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create directory");
    for entry in std::fs::read_dir(from).expect("read directory") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).expect("copy file");
        }
    }
}
