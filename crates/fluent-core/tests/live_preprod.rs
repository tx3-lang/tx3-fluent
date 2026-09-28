//! Prepares the fixture `transfer` against a live preprod TRP endpoint.
//!
//! Skips green unless `FLUENT_TRP_URL_PREPROD` is set. When it is, the test
//! also needs:
//!
//! - `TEST_PARTY_A_ADDRESS`: a funded preprod address, the sender;
//! - `TEST_PARTY_B_ADDRESS`: a preprod address, the receiver;
//! - `FLUENT_TRP_API_KEY_PREPROD` when the endpoint needs a key (sent as
//!   `dmtr-api-key`).
//!
//! A set URL with a missing party address fails rather than skipping. The
//! transaction is resolved only: nothing is signed or submitted, so no key
//! or mnemonic is read.
//!
//! ```sh
//! FLUENT_TRP_URL_PREPROD=https://cardano-preprod.trp-m1.demeter.run \
//! FLUENT_TRP_API_KEY_PREPROD=… TEST_PARTY_A_ADDRESS=addr_test1… \
//! TEST_PARTY_B_ADDRESS=addr_test1… \
//! cargo test -p fluent-core --test live_preprod -- --nocapture
//! ```

use std::env;
use std::path::PathBuf;

use fluent_core::envelope::OutputSummary;
use fluent_core::registration::load_dir;
use fluent_core::{Config, Engine, PrepareRequest};
use serde_json::json;

const API_KEY_VAR: &str = "FLUENT_TRP_API_KEY_PREPROD";
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

#[tokio::test]
async fn prepares_a_transfer_on_preprod() {
    let Some(trp_url) = set("FLUENT_TRP_URL_PREPROD") else {
        eprintln!("skipping: FLUENT_TRP_URL_PREPROD is not set");
        return;
    };
    let sender = required("TEST_PARTY_A_ADDRESS");
    let receiver = required("TEST_PARTY_B_ADDRESS");

    let text = format!(
        r#"
        [registrations]
        dir = "unused"
        [networks.preprod]
        trp_url = {}
        trp_api_key_env = "{API_KEY_VAR}"
        [limits]
        resolver_timeout_secs = 60
        global_cutoff_secs = 60
        [auth]
        mode = "none"
        "#,
        toml::Value::String(trp_url)
    );
    let key: Vec<(&str, String)> = set(API_KEY_VAR)
        .map(|key| (API_KEY_VAR, key))
        .into_iter()
        .collect();
    let config = Config::from_toml_str(&text, key).expect("live configuration");
    let registrations =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/registrations/valid");
    let engine = Engine::new(&config, &load_dir(registrations).unwrap().catalog);

    let prepared = engine
        .prepare(PrepareRequest {
            registration: "transfer_preprod".into(),
            tx: "transfer".into(),
            args: json!({
                "quantity": QUANTITY,
                "sender": sender,
                "receiver": receiver,
                "middleman": sender
            }),
        })
        .await
        .unwrap_or_else(|err| {
            panic!(
                "{} {}: {}",
                err.code(),
                err.message(),
                err.details().unwrap_or_default()
            )
        });

    assert_eq!(prepared.network, "preprod");
    assert_eq!(prepared.tx_hash.len(), 64);
    assert!(!prepared.summary.inputs.is_empty());
    // `transfer` declares the receiver's output first, and outputs keep their
    // declared order. The position identifies it even when the receiver is
    // also the sender, whose middleman and change outputs share its address.
    let outputs = &prepared.summary.outputs;
    assert_eq!(outputs.len(), 3, "{outputs:?}");
    assert_eq!(
        outputs[0],
        OutputSummary {
            address: receiver,
            lovelace: QUANTITY,
            assets: Vec::new(),
        }
    );
    eprintln!(
        "prepared {} ({} inputs, {} outputs, fee {} lovelace)",
        prepared.tx_hash,
        prepared.summary.inputs.len(),
        prepared.summary.outputs.len(),
        prepared.summary.fee_lovelace
    );
}
