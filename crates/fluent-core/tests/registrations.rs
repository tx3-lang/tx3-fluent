//! Registration bundles in `tests/fixtures/registrations/` load, or are
//! rejected by the rule they break.

use std::fs;
use std::path::{Path, PathBuf};

use fluent_core::ErrorCode;
use fluent_core::registration::{Loaded, Network, Registration, load_dir};

fn fixtures(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/registrations")
        .join(dir)
}

async fn load(dir: &Path) -> Loaded {
    load_dir(dir)
        .await
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
}

/// Values computed outside Fluent, from the fixture directory:
///
/// ```sh
/// t="sha256:$(sha256sum $slug/$tii | cut -d' ' -f1)"
/// s="sha256:$(sha256sum $slug/SKILL.md | cut -d' ' -f1)"
/// printf '%s' "$t$s$profile$network" | sha256sum | cut -c1-12
/// ```
struct Expected {
    slug: &'static str,
    protocol: &'static str,
    profile: &'static str,
    network: Network,
    tii_digest: &'static str,
    skill_digest: &'static str,
    revision: &'static str,
}

const VALID: [Expected; 2] = [
    Expected {
        slug: "strike_staking_mainnet",
        protocol: "strike-finance/strike-staking:0.1.0",
        profile: "mainnet",
        network: Network::Mainnet,
        tii_digest: "sha256:8da7d6658f6083325a644b7e1c6af6dc57494a7baf1addd7ed624499a2064bd3",
        skill_digest: "sha256:17744829d0f6f415d6c3f5a433d6568987016508edc6a0f170145a3779f128ae",
        revision: "ec7797deaa59",
    },
    Expected {
        slug: "transfer_preprod",
        protocol: "unknown/unknown:0.0.1",
        profile: "preprod",
        network: Network::Preprod,
        tii_digest: "sha256:8d5d715f300e618373b588af96f0cfb9b0a3904b3a34fc0038f72ebb1695da31",
        skill_digest: "sha256:95afa7141036ade51321df3e8f203727240f0bebd14fd583c0a7be7d56fbe0cd",
        revision: "04948dcd8aa4",
    },
];

fn assert_matches(registration: &Registration, expected: &Expected) {
    assert_eq!(registration.slug(), expected.slug);
    assert_eq!(registration.protocol_id(), expected.protocol);
    assert_eq!(registration.profile(), expected.profile);
    assert_eq!(registration.network(), expected.network);
    assert_eq!(registration.tii_digest(), expected.tii_digest);
    assert_eq!(registration.skill_digest(), expected.skill_digest);
    assert_eq!(registration.revision(), expected.revision);
}

#[tokio::test]
async fn valid_bundles_load_with_independently_computed_digests() {
    let dir = fixtures("valid");
    let loaded = load(&dir).await;
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);

    let catalog = &loaded.catalog;
    assert_eq!(catalog.len(), 2);
    let slugs: Vec<&str> = catalog.iter().map(|r| r.slug()).collect();
    assert_eq!(slugs, ["strike_staking_mainnet", "transfer_preprod"]);
    for expected in &VALID {
        let registration = catalog.get(expected.slug).unwrap();
        assert_matches(registration, expected);
        assert_eq!(registration.bundle(), dir.join(expected.slug));
        assert_matches(
            &Registration::load(dir.join(expected.slug)).await.unwrap(),
            expected,
        );
    }
    assert!(catalog.get("transfer").is_none());
}

#[tokio::test]
async fn loaded_registrations_expose_tii_and_skill() {
    let loaded = load(&fixtures("valid")).await;

    let strike = loaded.catalog.get("strike_staking_mainnet").unwrap();
    let mut txs: Vec<&str> = strike
        .tii()
        .transactions
        .keys()
        .map(String::as_str)
        .collect();
    txs.sort_unstable();
    assert_eq!(txs, ["add_stake", "stake", "withdraw_stake"]);
    assert!(
        strike.tii().profiles["mainnet"]
            .parties
            .contains_key("stakingscript")
    );
    assert_eq!(strike.manifest().tii_path(), "strike-staking.tii");
    let skill = strike.skill();
    assert_eq!(skill.protocol, "strike-finance/strike-staking:0.1.0");
    assert_eq!(skill.revision, 1);
    assert_eq!(skill.dependencies[0].id, "strike_balance");
    assert_eq!(skill.dependencies[0].required_for, ["stake", "add_stake"]);
    assert!(skill.body.starts_with("# strike-staking-mainnet\n"));

    let transfer = loaded.catalog.get("transfer_preprod").unwrap();
    assert!(transfer.protocol().txs().contains_key("transfer"));
    assert_eq!(transfer.tii().protocol.name, "unknown");
}

#[tokio::test]
async fn every_invalid_fixture_is_rejected_with_its_rule() {
    let dir = fixtures("invalid");
    let loaded = load(&dir).await;
    assert!(loaded.catalog.is_empty());

    let expected = [
        (
            "local_profile",
            ErrorCode::RegistrationUnavailable,
            "profile `local` cannot be deployed",
        ),
        (
            "malformed_tii",
            ErrorCode::RegistrationUnavailable,
            "malformed TII: missing field `transactions`",
        ),
        (
            "missing_profile",
            ErrorCode::RegistrationUnavailable,
            "missing profile: `preview` is not defined in the TII (defined: local, preprod)",
        ),
        (
            "wrong_network",
            ErrorCode::NetworkMismatch,
            "declares network mainnet, but its deployment profile serves preprod",
        ),
        (
            "wrong_skill_binding",
            ErrorCode::RegistrationUnavailable,
            "incompatible skill binding: skill tii_digest is \
             sha256:8da7d6658f6083325a644b7e1c6af6dc57494a7baf1addd7ed624499a2064bd3, \
             the TII is sha256:8d5d715f300e618373b588af96f0cfb9b0a3904b3a34fc0038f72ebb1695da31",
        ),
    ];
    assert_eq!(
        loaded.rejected.len(),
        expected.len(),
        "{:?}",
        loaded.rejected
    );
    for (rejection, (name, code, rule)) in loaded.rejected.iter().zip(expected) {
        let bundle = dir.join(name);
        let message = rejection.error.message();
        assert_eq!(rejection.bundle, bundle);
        assert_eq!(rejection.error.code(), code, "{name}: {message}");
        assert!(
            message.contains(&bundle.display().to_string()),
            "{name}: {message}"
        );
        assert!(message.contains(rule), "{name}: {message}");
    }
}

#[tokio::test]
async fn duplicate_slugs_reject_every_claimant() {
    let dir = fixtures("duplicate_slug");
    let loaded = load(&dir).await;
    assert!(loaded.catalog.is_empty());
    assert_eq!(loaded.rejected.len(), 2, "{:?}", loaded.rejected);

    for (rejection, (own, other)) in loaded
        .rejected
        .iter()
        .zip([("first", "second"), ("second", "first")])
    {
        let message = rejection.error.message();
        assert_eq!(rejection.bundle, dir.join(own));
        assert_eq!(rejection.error.code(), ErrorCode::RegistrationUnavailable);
        assert!(
            message.starts_with(&format!(
                "registration {} is unavailable: duplicate slug `transfer_preprod`, also used by",
                dir.join(own).display()
            )),
            "{message}"
        );
        assert!(
            message.ends_with(&dir.join(other).display().to_string()),
            "{message}"
        );
    }
}

/// A fresh, empty registrations directory under the test target directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("registrations")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clear scratch directory");
    }
    fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Copies the valid transfer bundle into `dir/name`.
fn transfer_copy(dir: &Path, name: &str) -> PathBuf {
    let source = fixtures("valid/transfer_preprod");
    let bundle = dir.join(name);
    fs::create_dir_all(&bundle).expect("create bundle directory");
    for entry in fs::read_dir(source).expect("read transfer fixture") {
        let entry = entry.expect("read transfer fixture entry");
        fs::copy(entry.path(), bundle.join(entry.file_name())).expect("copy fixture file");
    }
    bundle
}

fn edit(path: PathBuf, from: &str, to: &str) {
    let text = fs::read_to_string(&path).expect("read bundle file");
    assert!(text.contains(from), "{} lacks {from}", path.display());
    fs::write(&path, text.replacen(from, to, 1)).expect("write bundle file");
}

async fn rejection(bundle: &Path) -> String {
    let Err(err) = Registration::load(bundle).await else {
        panic!("{} loaded", bundle.display());
    };
    let message = err.message();
    assert_eq!(err.code(), ErrorCode::RegistrationUnavailable, "{message}");
    assert!(message.contains(&bundle.display().to_string()), "{message}");
    message
}

#[tokio::test]
async fn rejects_bundles_that_break_the_other_rules() {
    let dir = scratch("other-rules");
    let digest = "sha256:8d5d715f300e618373b588af96f0cfb9b0a3904b3a34fc0038f72ebb1695da31";

    let bundle = transfer_copy(&dir, "tii_digest");
    edit(bundle.join("registration.toml"), "8d5d715f", "0d5d715f");
    assert!(
        rejection(&bundle)
            .await
            .contains("TII digest mismatch: registration.toml declares sha256:0d5d715f")
    );

    let bundle = transfer_copy(&dir, "protocol");
    edit(bundle.join("registration.toml"), "\"0.0.1\"", "\"0.0.2\"");
    assert!(rejection(&bundle).await.contains(
        "protocol mismatch: registration.toml declares unknown/unknown:0.0.2, \
         the TII declares unknown/unknown:0.0.1"
    ));

    let bundle = transfer_copy(&dir, "manifest");
    edit(
        bundle.join("registration.toml"),
        "[skill]",
        "[skill]\nsha = 1",
    );
    let message = rejection(&bundle).await;
    assert!(
        message.contains("invalid registration.toml: line 15: unknown field `sha`"),
        "{message}"
    );

    let bundle = transfer_copy(&dir, "slug");
    edit(
        bundle.join("registration.toml"),
        "\"transfer_preprod\"",
        "\"T\"",
    );
    assert!(
        rejection(&bundle)
            .await
            .contains("invalid registration.toml: slug `T` must match")
    );

    let bundle = transfer_copy(&dir, "escape");
    edit(
        bundle.join("registration.toml"),
        "\"SKILL.md\"",
        "\"../SKILL.md\"",
    );
    assert!(
        rejection(&bundle)
            .await
            .contains("skill.path must be a relative path inside the bundle")
    );

    let bundle = transfer_copy(&dir, "no_manifest");
    fs::remove_file(bundle.join("registration.toml")).unwrap();
    assert!(
        rejection(&bundle)
            .await
            .contains("cannot read registration.toml")
    );

    let bundle = transfer_copy(&dir, "no_tii");
    fs::remove_file(bundle.join("protocol.tii")).unwrap();
    assert!(
        rejection(&bundle)
            .await
            .contains("cannot read TII protocol.tii")
    );

    let bundle = transfer_copy(&dir, "no_skill");
    fs::remove_file(bundle.join("SKILL.md")).unwrap();
    assert!(
        rejection(&bundle)
            .await
            .contains("cannot read skill SKILL.md")
    );

    let bundle = transfer_copy(&dir, "no_frontmatter");
    fs::write(bundle.join("SKILL.md"), "# Transfer\n").unwrap();
    assert!(
        rejection(&bundle)
            .await
            .contains("invalid skill SKILL.md: must start with YAML frontmatter")
    );

    let bundle = transfer_copy(&dir, "skill_fields");
    edit(bundle.join("SKILL.md"), "revision: 1", "revision: first");
    let message = rejection(&bundle).await;
    assert!(
        message.contains("invalid skill SKILL.md: invalid frontmatter:"),
        "{message}"
    );

    let bundle = transfer_copy(&dir, "skill_binding");
    edit(
        bundle.join("SKILL.md"),
        "unknown/unknown:0.0.1",
        "acme/transfer:0.1.0",
    );
    edit(
        bundle.join("SKILL.md"),
        "network: preprod",
        "network: preview",
    );
    edit(bundle.join("SKILL.md"), "[transfer]", "[transfer, swap]");
    assert!(rejection(&bundle).await.contains(
        "incompatible skill binding: skill protocol is acme/transfer:0.1.0, the registration is \
         unknown/unknown:0.0.1; skill network is preview, the registration serves preprod; \
         skill dependency `sender_address` is required for `swap`, which the TII does not define"
    ));

    // A skill bound to the right TII, but the manifest omits the optional
    // digest: still loads.
    let bundle = transfer_copy(&dir, "no_declared_digest");
    edit(
        bundle.join("registration.toml"),
        &format!("tii_digest = \"{digest}\""),
        "",
    );
    Registration::load(&bundle).await.unwrap();
}

#[tokio::test]
async fn registry_bundles_must_not_carry_a_tii() {
    let dir = scratch("registry");
    let bundle = transfer_copy(&dir, "registry");
    edit(
        bundle.join("registration.toml"),
        "source = \"local\"",
        "source = \"registry\"\n\n[protocol.registry]\nurl = \"http://127.0.0.1:9\"\n\
         ref = \"open-tx3/transfer:0.1.0\"\nmanifest_digest = \
         \"sha256:b7b32dfa0fb9eda772700f8ba01a4d76084b2ea2e5883cf721db2505c6b35e77\"",
    );
    // Rejected before any fetch; nothing listens on the registry URL anyway.
    assert!(rejection(&bundle).await.contains(
        "a registry-sourced bundle must not carry protocol.tii: its TII comes only from the \
         registry"
    ));
}

#[tokio::test]
async fn a_rejected_bundle_does_not_stop_the_others() {
    let dir = scratch("mixed");
    transfer_copy(&dir, "good");
    let bad = transfer_copy(&dir, "bad");
    edit(
        bad.join("registration.toml"),
        "\"transfer_preprod\"",
        "\"transfer_other\"",
    );
    fs::write(bad.join("protocol.tii"), "not json").unwrap();
    fs::create_dir_all(dir.join(".cache")).unwrap();
    fs::write(dir.join(".cache/ignored.tii"), "not json").unwrap();
    fs::write(dir.join("README.md"), "Registrations for this host.\n").unwrap();

    let loaded = load(&dir).await;
    let slugs: Vec<&str> = loaded.catalog.iter().map(|r| r.slug()).collect();
    assert_eq!(slugs, ["transfer_preprod"]);
    assert_eq!(loaded.rejected.len(), 1);
    assert_eq!(loaded.rejected[0].bundle, bad);
    assert!(
        loaded.rejected[0]
            .error
            .message()
            .contains("TII digest mismatch")
    );
}

#[tokio::test]
async fn an_unreadable_directory_fails_the_load() {
    let dir = fixtures("does-not-exist");
    let err = load_dir(&dir).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::RegistrationUnavailable);
    let message = err.message();
    assert!(message.contains(&dir.display().to_string()), "{message}");
    assert!(
        message.contains("cannot read the registrations directory"),
        "{message}"
    );
}
