//! Registry-sourced registrations against a fake OCI registry: the TII is
//! fetched by reference, verified against the pinned manifest digest and the
//! layer digest, and cached by manifest digest.
//!
//! The artifact wraps the transfer fixture's TII, so a registry bundle made
//! from the transfer bundle keeps its skill binding and revision.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fluent_core::ErrorCode;
use fluent_core::registration::{Loaded, Registration, load_dir, load_dir_with, sha256_digest};
use fluent_core::registry::{OciFetcher, PROTOCOL_MEDIA_TYPE, REVISION_ANNOTATION, TII_MEDIA_TYPE};
use serde_json::{Value, json};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPOSITORY: &str = "unknown/unknown";
const TAG: &str = "0.0.1";
const REVISION: &str = "6bcfa90f700639730f7e83e1fa99d226f041f062";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// The transfer bundle's revision; the registry copy binds the same TII,
/// skill, profile and network.
const TRANSFER_REVISION: &str = "04948dcd8aa4";

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/registrations")
        .join(path)
}

fn transfer_tii() -> Vec<u8> {
    fs::read(fixture("valid/transfer_preprod/protocol.tii")).expect("read the transfer TII")
}

/// A published artifact: its manifest and the blobs it names.
struct Artifact {
    manifest: Vec<u8>,
    tii: Vec<u8>,
    source: Vec<u8>,
}

impl Artifact {
    /// A Tx3 source layer, the transfer TII layer and a revision annotation.
    fn transfer() -> Artifact {
        let tii = transfer_tii();
        let source = b"tx transfer() {}\n".to_vec();
        let layers = json!([
            layer(PROTOCOL_MEDIA_TYPE, &source),
            layer(TII_MEDIA_TYPE, &tii),
            layer("text/markdown", b"# transfer\n"),
        ]);
        Artifact {
            manifest: manifest(layers),
            tii,
            source,
        }
    }

    fn manifest_digest(&self) -> String {
        sha256_digest(&self.manifest)
    }
}

fn layer(media_type: &str, bytes: &[u8]) -> Value {
    json!({ "mediaType": media_type, "digest": sha256_digest(bytes), "size": bytes.len() })
}

fn manifest(layers: Value) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": sha256_digest(b"{}"),
            "size": 2
        },
        "layers": layers,
        "annotations": {
            REVISION_ANNOTATION: REVISION,
            "org.opencontainers.image.title": "transfer"
        }
    }))
    .expect("serialize the manifest")
}

/// A registry serving `manifest` at `REPOSITORY:TAG` and each blob at its
/// digest.
async fn registry(manifest: &[u8], blobs: &[&[u8]]) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v2/{REPOSITORY}/manifests/{TAG}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(manifest.to_vec(), MANIFEST_MEDIA_TYPE),
        )
        .mount(&server)
        .await;
    for blob in blobs {
        serve_blob(&server, &sha256_digest(blob), blob).await;
    }
    server
}

async fn serve_blob(server: &MockServer, digest: &str, bytes: &[u8]) {
    Mock::given(method("GET"))
        .and(path(format!("/v2/{REPOSITORY}/blobs/{digest}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(bytes.to_vec(), "application/octet-stream"),
        )
        .mount(server)
        .await;
}

/// A fresh, empty registrations directory under the test target directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("registry")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clear scratch directory");
    }
    fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Writes the transfer bundle as a registry bundle in `dir/transfer`, pinned
/// to `manifest_digest` at `url`.
fn registry_bundle(dir: &Path, url: &str, manifest_digest: &str) -> PathBuf {
    let source = fixture("valid/transfer_preprod");
    let bundle = dir.join("transfer");
    fs::create_dir_all(&bundle).expect("create bundle directory");
    fs::copy(source.join("SKILL.md"), bundle.join("SKILL.md")).expect("copy the skill");
    let manifest = fs::read_to_string(source.join("registration.toml"))
        .expect("read the transfer manifest")
        .replacen(
            "source = \"local\"",
            &format!(
                "source = \"registry\"\n\n[protocol.registry]\nurl = \"{url}\"\n\
                 ref = \"{REPOSITORY}:{TAG}\"\nmanifest_digest = \"{manifest_digest}\""
            ),
            1,
        );
    fs::write(bundle.join("registration.toml"), manifest).expect("write the manifest");
    bundle
}

fn cache_file(dir: &Path, manifest_digest: &str, extension: &str) -> PathBuf {
    dir.join(".cache")
        .join(format!("{manifest_digest}.{extension}"))
}

/// The single rejection's message, checked for code and bundle path.
fn only_rejection(loaded: &Loaded, bundle: &Path) -> String {
    assert!(loaded.catalog.is_empty(), "{:?}", loaded.catalog);
    assert_eq!(loaded.rejected.len(), 1, "{:?}", loaded.rejected);
    let rejection = &loaded.rejected[0];
    let message = rejection.error.message();
    assert_eq!(rejection.bundle, bundle);
    assert_eq!(
        rejection.error.code(),
        ErrorCode::RegistrationUnavailable,
        "{message}"
    );
    assert!(message.contains(&bundle.display().to_string()), "{message}");
    message
}

async fn load(dir: &Path) -> Loaded {
    load_dir(dir)
        .await
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
}

#[tokio::test]
async fn fetches_verifies_and_caches_the_pinned_artifact() {
    let artifact = Artifact::transfer();
    let server = registry(&artifact.manifest, &[&artifact.tii]).await;
    let dir = scratch("happy");
    let digest = artifact.manifest_digest();
    registry_bundle(&dir, &server.uri(), &digest);

    let loaded = load(&dir).await;
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    let registration = loaded.catalog.get("transfer_preprod").unwrap();
    assert_eq!(registration.tii_digest(), sha256_digest(&artifact.tii));
    assert_eq!(registration.revision(), TRANSFER_REVISION);
    assert!(registration.protocol().txs().contains_key("transfer"));

    let provenance = registration.provenance().unwrap();
    assert_eq!(provenance.registry_url, server.uri());
    assert_eq!(provenance.reference, format!("{REPOSITORY}:{TAG}"));
    assert_eq!(provenance.manifest_digest, digest);
    assert_eq!(
        provenance.source_digest.as_deref(),
        Some(sha256_digest(&artifact.source).as_str())
    );
    assert_eq!(provenance.source_revision.as_deref(), Some(REVISION));

    assert_eq!(
        fs::read(cache_file(&dir, &digest, "tii")).unwrap(),
        artifact.tii
    );
    assert_eq!(
        fs::read(cache_file(&dir, &digest, "manifest.json")).unwrap(),
        artifact.manifest
    );
}

#[tokio::test]
async fn a_moved_reference_is_rejected_as_changed_content() {
    let artifact = Artifact::transfer();
    let server = registry(&artifact.manifest, &[&artifact.tii]).await;
    let dir = scratch("content-changed");
    let pinned = sha256_digest(b"the manifest published at registration time");
    let bundle = registry_bundle(&dir, &server.uri(), &pinned);

    let message = only_rejection(&load(&dir).await, &bundle);
    assert!(
        message.ends_with(&format!(
            "registry content changed: {REPOSITORY}:{TAG} at {} resolves to manifest {}, the \
             registration pins {pinned}",
            server.uri(),
            artifact.manifest_digest()
        )),
        "{message}"
    );
    assert!(!dir.join(".cache").exists());
}

#[tokio::test]
async fn an_artifact_without_a_tii_layer_is_rejected() {
    let source = b"tx transfer() {}\n";
    let manifest = manifest(json!([layer(PROTOCOL_MEDIA_TYPE, source)]));
    let server = registry(&manifest, &[source]).await;
    let dir = scratch("missing-tii-layer");
    let bundle = registry_bundle(&dir, &server.uri(), &sha256_digest(&manifest));

    let message = only_rejection(&load(&dir).await, &bundle);
    assert!(
        message.ends_with(&format!(
            "{REPOSITORY}:{TAG} at {} has no application/tii+json layer",
            server.uri()
        )),
        "{message}"
    );
}

#[tokio::test]
async fn a_blob_that_does_not_match_its_layer_digest_is_rejected() {
    let artifact = Artifact::transfer();
    let server = registry(&artifact.manifest, &[]).await;
    // Same length, one byte changed.
    let mut tampered = artifact.tii.clone();
    tampered[0] ^= 0x20;
    let declared = sha256_digest(&artifact.tii);
    serve_blob(&server, &declared, &tampered).await;
    let dir = scratch("blob-digest-mismatch");
    let bundle = registry_bundle(&dir, &server.uri(), &artifact.manifest_digest());

    let message = only_rejection(&load(&dir).await, &bundle);
    assert!(
        message.ends_with(&format!(
            "TII layer digest mismatch: the manifest of {REPOSITORY}:{TAG} at {} declares \
             {declared}, the registry sent {}",
            server.uri(),
            sha256_digest(&tampered)
        )),
        "{message}"
    );
    assert!(!dir.join(".cache").exists());
}

#[tokio::test]
async fn a_blob_longer_than_its_layer_is_rejected() {
    let artifact = Artifact::transfer();
    let server = registry(&artifact.manifest, &[]).await;
    let mut longer = artifact.tii.clone();
    longer.extend_from_slice(b"\n\n");
    serve_blob(&server, &sha256_digest(&artifact.tii), &longer).await;
    let dir = scratch("blob-size-mismatch");
    let bundle = registry_bundle(&dir, &server.uri(), &artifact.manifest_digest());

    let message = only_rejection(&load(&dir).await, &bundle);
    assert!(
        message.ends_with(&format!(
            "TII layer size mismatch: the manifest of {REPOSITORY}:{TAG} at {} declares {} \
             bytes, the registry sent more",
            server.uri(),
            artifact.tii.len()
        )),
        "{message}"
    );
}

#[tokio::test]
async fn a_verified_cache_is_used_without_any_request() {
    let artifact = Artifact::transfer();
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let dir = scratch("cache-hit");
    let digest = artifact.manifest_digest();
    registry_bundle(&dir, &server.uri(), &digest);
    fs::create_dir_all(dir.join(".cache")).unwrap();
    fs::write(
        cache_file(&dir, &digest, "manifest.json"),
        &artifact.manifest,
    )
    .unwrap();
    fs::write(cache_file(&dir, &digest, "tii"), &artifact.tii).unwrap();

    let loaded = load(&dir).await;
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    let registration = loaded.catalog.get("transfer_preprod").unwrap();
    assert_eq!(registration.revision(), TRANSFER_REVISION);
    let provenance = registration.provenance().unwrap();
    assert_eq!(provenance.source_revision.as_deref(), Some(REVISION));
    assert_eq!(
        provenance.source_digest.as_deref(),
        Some(sha256_digest(&artifact.source).as_str())
    );

    let registry = &registration.manifest().protocol.registry;
    let fetched = OciFetcher::new(dir.join(".cache"))
        .fetch_tii(registry.as_ref().unwrap())
        .await
        .unwrap();
    assert!(fetched.cached);
    assert_eq!(fetched.tii, artifact.tii);

    server.verify().await;
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_cache_that_does_not_verify_is_replaced() {
    let artifact = Artifact::transfer();
    let server = registry(&artifact.manifest, &[&artifact.tii]).await;
    let dir = scratch("cache-tampered");
    let digest = artifact.manifest_digest();
    registry_bundle(&dir, &server.uri(), &digest);
    fs::create_dir_all(dir.join(".cache")).unwrap();
    fs::write(
        cache_file(&dir, &digest, "manifest.json"),
        &artifact.manifest,
    )
    .unwrap();
    fs::write(cache_file(&dir, &digest, "tii"), b"{}").unwrap();

    let loaded = load(&dir).await;
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    assert!(!server.received_requests().await.unwrap().is_empty());
    assert_eq!(
        fs::read(cache_file(&dir, &digest, "tii")).unwrap(),
        artifact.tii
    );
}

#[tokio::test]
async fn a_slow_registry_times_out_and_rejects_only_its_registration() {
    let artifact = Artifact::transfer();
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;
    let dir = scratch("timeout");
    let bundle = registry_bundle(&dir, &server.uri(), &artifact.manifest_digest());
    // A local bundle beside it, under another slug.
    let local = dir.join("local");
    fs::create_dir_all(&local).unwrap();
    for entry in fs::read_dir(fixture("valid/transfer_preprod")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), local.join(entry.file_name())).unwrap();
    }
    let manifest = fs::read_to_string(local.join("registration.toml")).unwrap();
    fs::write(
        local.join("registration.toml"),
        manifest.replacen("\"transfer_preprod\"", "\"transfer_local\"", 1),
    )
    .unwrap();

    let fetcher = OciFetcher::new(dir.join(".cache")).with_timeout(Duration::from_millis(300));
    let loaded = load_dir_with(&dir, &fetcher).await.unwrap();
    let slugs: Vec<&str> = loaded.catalog.iter().map(|r| r.slug()).collect();
    assert_eq!(slugs, ["transfer_local"]);
    assert_eq!(loaded.rejected.len(), 1, "{:?}", loaded.rejected);
    assert_eq!(loaded.rejected[0].bundle, bundle);
    let message = loaded.rejected[0].error.message();
    assert!(
        message.ends_with(&format!(
            "fetching {REPOSITORY}:{TAG} at {} timed out after 300ms",
            server.uri()
        )),
        "{message}"
    );
}

#[tokio::test]
async fn an_unreachable_registry_is_reported() {
    // Nothing listens on the discard port.
    let dir = scratch("unreachable");
    let bundle = registry_bundle(&dir, "http://127.0.0.1:9", &sha256_digest(b"manifest"));

    let Err(err) = Registration::load(&bundle).await else {
        panic!("{} loaded", bundle.display());
    };
    let message = err.message();
    assert_eq!(err.code(), ErrorCode::RegistrationUnavailable, "{message}");
    assert!(
        message.contains(&format!(
            "cannot fetch {REPOSITORY}:{TAG} at http://127.0.0.1:9: "
        )),
        "{message}"
    );
}
