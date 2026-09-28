//! Registry artifacts: the TII of a `source = "registry"` registration,
//! fetched from an OCI registry and bound to the manifest digest the
//! registration pins.
//!
//! [`OciFetcher::fetch_tii`] resolves `ref` at the registry `url`
//! anonymously, then:
//!
//! 1. requires the SHA-256 of the manifest bytes to equal `manifest_digest`
//!    (otherwise "registry content changed");
//! 2. takes the manifest's one [`TII_MEDIA_TYPE`] layer;
//! 3. pulls that layer's blob and requires its SHA-256 to equal the layer
//!    digest;
//! 4. records the [`PROTOCOL_MEDIA_TYPE`] layer digest and the
//!    [`REVISION_ANNOTATION`] annotation as [`Provenance`];
//! 5. caches the manifest and the TII in the fetcher's cache directory as
//!    `{manifest_digest}.manifest.json` and `{manifest_digest}.tii`.
//!
//! A cached pair is used without any request when the manifest hashes to the
//! pinned digest and the TII hashes to that manifest's TII layer digest.
//! Anything else in the cache is ignored and replaced by a fresh fetch.
//!
//! Provenance records where the bytes came from. It is not a verified
//! publisher identity.

use std::error::Error as StdError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::DigestError;
use oci_client::manifest::{
    IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest,
};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};

use crate::registration::{RegistrySource, is_sha256_digest, sha256_digest};

/// Media type of the TII layer.
pub const TII_MEDIA_TYPE: &str = "application/tii+json";

/// Media type of the Tx3 source layer.
pub const PROTOCOL_MEDIA_TYPE: &str = "application/tx3";

/// Manifest annotation holding the source revision the artifact was built
/// from.
pub const REVISION_ANNOTATION: &str = "org.opencontainers.image.revision";

/// Name of the cache directory inside `[registrations].dir`.
pub const CACHE_DIR: &str = ".cache";

/// How long one artifact fetch may take unless the fetcher says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Largest TII layer a manifest may declare.
pub const MAX_TII_BYTES: u64 = 8 * 1024 * 1024;

/// Fetches registry artifacts, caching them by manifest digest.
#[derive(Debug, Clone)]
pub struct OciFetcher {
    cache_dir: PathBuf,
    timeout: Duration,
}

/// A registry artifact's TII, verified against the pinned manifest digest.
#[derive(Debug, Clone)]
pub struct FetchedArtifact {
    /// The TII bytes, which hash to the manifest's TII layer digest.
    pub tii: Vec<u8>,
    /// Where the bytes came from.
    pub provenance: Provenance,
    /// Whether the bytes came from the cache rather than the registry.
    pub cached: bool,
}

/// Where a registry artifact came from. Recorded, not verified: it does not
/// identify the publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Registry base URL, as the registration names it.
    pub registry_url: String,
    /// Artifact reference, as the registration names it.
    pub reference: String,
    /// Content digest of the artifact manifest, `sha256:<hex>`.
    pub manifest_digest: String,
    /// Digest of the manifest's [`PROTOCOL_MEDIA_TYPE`] layer, if it has one.
    pub source_digest: Option<String>,
    /// The manifest's [`REVISION_ANNOTATION`], if it has one.
    pub source_revision: Option<String>,
}

/// Why a registry artifact could not be used. Each message names the
/// artifact as `{ref} at {url}`.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The registry URL and the reference do not address an artifact.
    #[error("cannot address {artifact}: {reason}")]
    InvalidReference {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// What is wrong with the URL or the reference.
        reason: String,
    },
    /// The registry could not be reached or answered with an error.
    #[error("cannot fetch {artifact}: {reason}")]
    Unreachable {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// The transport or registry error.
        reason: String,
    },
    /// The fetch did not finish within the fetcher's timeout.
    #[error("fetching {artifact} timed out after {after:?}")]
    Timeout {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// The timeout that elapsed.
        after: Duration,
    },
    /// The reference resolves to a manifest other than the pinned one.
    #[error(
        "registry content changed: {artifact} resolves to manifest {actual}, the registration \
         pins {pinned}"
    )]
    ContentChanged {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// The manifest digest the registration pins.
        pinned: String,
        /// The digest of the manifest the registry returned.
        actual: String,
    },
    /// The manifest is not an image manifest Fluent can use.
    #[error("invalid manifest for {artifact}: {reason}")]
    InvalidManifest {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// What is wrong with the manifest.
        reason: String,
    },
    /// The manifest has no [`TII_MEDIA_TYPE`] layer.
    #[error("{artifact} has no {TII_MEDIA_TYPE} layer")]
    MissingTiiLayer {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
    },
    /// The registry sent more TII bytes than the manifest declares.
    #[error(
        "TII layer size mismatch: the manifest of {artifact} declares {declared} bytes, the \
         registry sent more"
    )]
    BlobSizeMismatch {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// The layer size the manifest declares.
        declared: u64,
    },
    /// The TII bytes do not hash to the layer digest.
    #[error(
        "TII layer digest mismatch: the manifest of {artifact} declares {expected}, the registry \
         sent {actual}"
    )]
    BlobDigestMismatch {
        /// The artifact, as `{ref} at {url}`.
        artifact: String,
        /// The layer digest the manifest declares.
        expected: String,
        /// The digest of the bytes the registry sent.
        actual: String,
    },
}

/// What a manifest that hashes to the pinned digest says about its artifact.
#[derive(Debug)]
struct ManifestFacts {
    tii_layer: OciDescriptor,
    source_digest: Option<String>,
    source_revision: Option<String>,
}

impl OciFetcher {
    /// A fetcher caching in `cache_dir`, with [`DEFAULT_TIMEOUT`]. The
    /// directory is created on the first write.
    pub fn new(cache_dir: impl Into<PathBuf>) -> OciFetcher {
        OciFetcher {
            cache_dir: cache_dir.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// The same fetcher with a different per-artifact timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> OciFetcher {
        self.timeout = timeout;
        self
    }

    /// The cache directory.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// How long one artifact fetch may take.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The TII of the artifact `source` pins, from the cache when a verified
    /// copy is there and from the registry otherwise; see the
    /// [module documentation](self).
    pub async fn fetch_tii(&self, source: &RegistrySource) -> Result<FetchedArtifact, FetchError> {
        let (reference, protocol) = reference(source)?;
        if let Some(cached) = self.read_cache(source) {
            tracing::debug!(
                reference = %source.reference,
                manifest_digest = %source.manifest_digest,
                "using cached registry artifact"
            );
            return Ok(cached);
        }

        let (manifest, facts, tii) =
            tokio::time::timeout(self.timeout, pull(source, &reference, protocol))
                .await
                .map_err(|_| FetchError::Timeout {
                    artifact: artifact(source),
                    after: self.timeout,
                })??;
        tracing::info!(
            reference = %source.reference,
            registry = %source.url,
            manifest_digest = %source.manifest_digest,
            "fetched registry artifact"
        );
        self.write_cache(&source.manifest_digest, &manifest, &tii);
        Ok(FetchedArtifact {
            tii,
            provenance: provenance(source, facts),
            cached: false,
        })
    }

    fn cache_paths(&self, manifest_digest: &str) -> (PathBuf, PathBuf) {
        (
            self.cache_dir
                .join(format!("{manifest_digest}.manifest.json")),
            self.cache_dir.join(format!("{manifest_digest}.tii")),
        )
    }

    /// The cached artifact, if both files are present and verify.
    fn read_cache(&self, source: &RegistrySource) -> Option<FetchedArtifact> {
        let (manifest_path, tii_path) = self.cache_paths(&source.manifest_digest);
        let manifest = fs::read(&manifest_path).ok()?;
        let tii = fs::read(&tii_path).ok()?;
        let facts = match inspect_manifest(source, &manifest) {
            Ok(facts) => facts,
            Err(err) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    "ignoring cached manifest: {err}"
                );
                return None;
            }
        };
        let digest = sha256_digest(&tii);
        if digest != facts.tii_layer.digest {
            tracing::warn!(
                path = %tii_path.display(),
                "ignoring cached TII: its digest is {digest}, the manifest declares {}",
                facts.tii_layer.digest
            );
            return None;
        }
        Some(FetchedArtifact {
            tii,
            provenance: provenance(source, facts),
            cached: true,
        })
    }

    /// Caches a verified artifact. A failure is logged, not returned: the
    /// bytes are already verified, and the next start fetches them again.
    fn write_cache(&self, manifest_digest: &str, manifest: &[u8], tii: &[u8]) {
        let (manifest_path, tii_path) = self.cache_paths(manifest_digest);
        let written = fs::create_dir_all(&self.cache_dir)
            .and_then(|()| write_atomically(&tii_path, tii))
            .and_then(|()| write_atomically(&manifest_path, manifest));
        if let Err(err) = written {
            tracing::warn!(
                cache = %self.cache_dir.display(),
                manifest_digest,
                "cannot cache registry artifact: {err}"
            );
        }
    }
}

/// The artifact as `{ref} at {url}`, for messages.
fn artifact(source: &RegistrySource) -> String {
    format!("{} at {}", source.reference, source.url)
}

fn provenance(source: &RegistrySource, facts: ManifestFacts) -> Provenance {
    Provenance {
        registry_url: source.url.clone(),
        reference: source.reference.clone(),
        manifest_digest: source.manifest_digest.clone(),
        source_digest: facts.source_digest,
        source_revision: facts.source_revision,
    }
}

/// The OCI reference `{host}/{ref}` and the protocol to reach `host` with.
/// The URL must name only a scheme, a host and an optional port.
fn reference(source: &RegistrySource) -> Result<(Reference, ClientProtocol), FetchError> {
    let invalid = |reason: String| FetchError::InvalidReference {
        artifact: artifact(source),
        reason,
    };
    let (protocol, rest) = if let Some(rest) = source.url.strip_prefix("https://") {
        (ClientProtocol::Https, rest)
    } else if let Some(rest) = source.url.strip_prefix("http://") {
        (ClientProtocol::Http, rest)
    } else {
        return Err(invalid(
            "the registry URL must be http:// or https://".to_string(),
        ));
    };
    let host = rest.strip_suffix('/').unwrap_or(rest);
    if host.is_empty() || host.contains(['/', '?', '#', '@']) {
        return Err(invalid(
            "the registry URL must name only a host and an optional port".to_string(),
        ));
    }
    let reference = Reference::try_from(format!("{host}/{}", source.reference))
        .map_err(|err| invalid(format!("invalid reference: {err}")))?;
    // A host without a dot or a port reads as a Docker Hub namespace.
    if reference.registry() != host {
        return Err(invalid(format!(
            "`{host}` is not read as a registry host; add a port or a domain"
        )));
    }
    Ok((reference, protocol))
}

/// Pulls and verifies the manifest and the TII from the registry.
async fn pull(
    source: &RegistrySource,
    reference: &Reference,
    protocol: ClientProtocol,
) -> Result<(Vec<u8>, ManifestFacts, Vec<u8>), FetchError> {
    let unreachable = |err: &dyn StdError| FetchError::Unreachable {
        artifact: artifact(source),
        reason: error_chain(err),
    };
    let client = Client::try_from(ClientConfig {
        protocol,
        // Registry artifacts are single manifests, never image indexes.
        platform_resolver: None,
        ..ClientConfig::default()
    })
    .map_err(|err| unreachable(&err))?;

    let (manifest, _) = client
        .pull_manifest_raw(
            reference,
            &RegistryAuth::Anonymous,
            &[OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE],
        )
        .await
        .map_err(|err| unreachable(&err))?;
    let facts = inspect_manifest(source, &manifest)?;

    let layer = &facts.tii_layer;
    // `inspect_manifest` bounds the size by `MAX_TII_BYTES`.
    let declared = u64::try_from(layer.size).unwrap_or_default();
    let mut stream = client
        .pull_blob_stream(reference, layer)
        .await
        .map_err(|err| unreachable(&err))?;
    let mut tii = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if (tii.len() + bytes.len()) as u64 > declared {
                    return Err(FetchError::BlobSizeMismatch {
                        artifact: artifact(source),
                        declared,
                    });
                }
                tii.extend_from_slice(&bytes);
            }
            // The client's own digest check, raised once every byte has
            // arrived; the comparison below reports it.
            Err(err) if err.get_ref().is_some_and(|inner| inner.is::<DigestError>()) => break,
            Err(err) => return Err(unreachable(&err)),
        }
    }
    let actual = sha256_digest(&tii);
    if actual != layer.digest {
        return Err(FetchError::BlobDigestMismatch {
            artifact: artifact(source),
            expected: layer.digest.clone(),
            actual,
        });
    }
    Ok((manifest.to_vec(), facts, tii))
}

/// Checks manifest bytes against the pinned digest and reads what Fluent
/// needs from them.
fn inspect_manifest(source: &RegistrySource, bytes: &[u8]) -> Result<ManifestFacts, FetchError> {
    let actual = sha256_digest(bytes);
    if actual != source.manifest_digest {
        return Err(FetchError::ContentChanged {
            artifact: artifact(source),
            pinned: source.manifest_digest.clone(),
            actual,
        });
    }
    let invalid = |reason: String| FetchError::InvalidManifest {
        artifact: artifact(source),
        reason,
    };

    let manifest: OciImageManifest = serde_json::from_slice(bytes)
        .map_err(|err| invalid(format!("not an image manifest: {err}")))?;
    if manifest.schema_version != 2 {
        return Err(invalid(format!(
            "schema version {} is not 2",
            manifest.schema_version
        )));
    }
    if let Some(media_type) = &manifest.media_type
        && media_type != OCI_IMAGE_MEDIA_TYPE
        && media_type != IMAGE_MANIFEST_MEDIA_TYPE
    {
        return Err(invalid(format!(
            "media type {media_type} is not an image manifest"
        )));
    }

    let mut tii_layers = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == TII_MEDIA_TYPE);
    let tii_layer = tii_layers
        .next()
        .ok_or_else(|| FetchError::MissingTiiLayer {
            artifact: artifact(source),
        })?;
    if tii_layers.next().is_some() {
        return Err(invalid(format!(
            "it has more than one {TII_MEDIA_TYPE} layer"
        )));
    }
    if !is_sha256_digest(&tii_layer.digest) {
        return Err(invalid(format!(
            "the {TII_MEDIA_TYPE} layer digest `{}` is not `sha256:` followed by 64 lowercase \
             hex digits",
            tii_layer.digest
        )));
    }
    if !u64::try_from(tii_layer.size).is_ok_and(|size| size <= MAX_TII_BYTES) {
        return Err(invalid(format!(
            "the {TII_MEDIA_TYPE} layer declares {} bytes; at most {MAX_TII_BYTES} are allowed",
            tii_layer.size
        )));
    }

    let source_digest = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == PROTOCOL_MEDIA_TYPE)
        .map(|layer| layer.digest.clone());
    let source_revision = manifest
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(REVISION_ANNOTATION))
        .cloned();
    Ok(ManifestFacts {
        tii_layer: tii_layer.clone(),
        source_digest,
        source_revision,
    })
}

/// `err` and each of its sources, joined by `: `.
fn error_chain(err: &dyn StdError) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let cause_message = cause.to_string();
        if !message.contains(&cause_message) {
            message.push_str(": ");
            message.push_str(&cause_message);
        }
        source = cause.source();
    }
    message
}

/// Writes `bytes` to a sibling temporary file, then renames it over `path`,
/// so readers never see a partial file.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TII_DIGEST: &str =
        "sha256:de15465a47728deb3483650e628301e9d19e9c91a584626d4672525bfcba5ca1";
    const SOURCE_DIGEST: &str =
        "sha256:9cd8fca341b497fbaa8e425cc4f73158e43c57eb52c03209e611c5e229947686";

    fn source(url: &str, reference: &str, manifest: &[u8]) -> RegistrySource {
        RegistrySource {
            url: url.to_string(),
            reference: reference.to_string(),
            manifest_digest: sha256_digest(manifest),
        }
    }

    fn manifest(layers: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MEDIA_TYPE,
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": sha256_digest(b"{}"),
                "size": 2
            },
            "layers": layers,
            "annotations": { REVISION_ANNOTATION: "6bcfa90f" }
        }))
        .unwrap()
    }

    fn layer(media_type: &str, digest: &str, size: u64) -> serde_json::Value {
        json!({ "mediaType": media_type, "digest": digest, "size": size })
    }

    fn inspect(bytes: &[u8]) -> Result<ManifestFacts, FetchError> {
        inspect_manifest(
            &source(
                "https://oci.tx3.land",
                "open-tx3/strike-staking:0.1.0",
                bytes,
            ),
            bytes,
        )
    }

    #[test]
    fn manifests_yield_the_tii_layer_and_provenance() {
        let bytes = manifest(json!([
            layer(PROTOCOL_MEDIA_TYPE, SOURCE_DIGEST, 7939),
            layer(TII_MEDIA_TYPE, TII_DIGEST, 20189),
            layer("text/markdown", SOURCE_DIGEST, 2946),
        ]));
        let facts = inspect(&bytes).unwrap();
        assert_eq!(facts.tii_layer.digest, TII_DIGEST);
        assert_eq!(facts.tii_layer.size, 20189);
        assert_eq!(facts.source_digest.as_deref(), Some(SOURCE_DIGEST));
        assert_eq!(facts.source_revision.as_deref(), Some("6bcfa90f"));
    }

    #[test]
    fn source_layer_and_revision_are_optional() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&manifest(json!([layer(TII_MEDIA_TYPE, TII_DIGEST, 1)])))
                .unwrap();
        value.as_object_mut().unwrap().remove("annotations");
        let bytes = serde_json::to_vec(&value).unwrap();
        let facts = inspect(&bytes).unwrap();
        assert_eq!(facts.source_digest, None);
        assert_eq!(facts.source_revision, None);
    }

    #[test]
    fn manifests_must_hash_to_the_pinned_digest() {
        let bytes = manifest(json!([layer(TII_MEDIA_TYPE, TII_DIGEST, 1)]));
        let pinned = source("https://oci.tx3.land", "a/b:1", b"other");
        let err = inspect_manifest(&pinned, &bytes).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "registry content changed: a/b:1 at https://oci.tx3.land resolves to manifest \
                 {}, the registration pins {}",
                sha256_digest(&bytes),
                sha256_digest(b"other")
            )
        );
    }

    #[test]
    fn manifests_without_one_usable_tii_layer_are_rejected() {
        let cases = [
            (
                manifest(json!([layer(PROTOCOL_MEDIA_TYPE, SOURCE_DIGEST, 1)])),
                "has no application/tii+json layer",
            ),
            (
                manifest(json!([
                    layer(TII_MEDIA_TYPE, TII_DIGEST, 1),
                    layer(TII_MEDIA_TYPE, SOURCE_DIGEST, 1),
                ])),
                "it has more than one application/tii+json layer",
            ),
            (
                manifest(json!([layer(TII_MEDIA_TYPE, "sha512:abc", 1)])),
                "layer digest `sha512:abc` is not `sha256:`",
            ),
            (
                manifest(json!([layer(
                    TII_MEDIA_TYPE,
                    TII_DIGEST,
                    MAX_TII_BYTES + 1
                )])),
                "layer declares 8388609 bytes; at most 8388608 are allowed",
            ),
            (
                String::from_utf8(manifest(json!([layer(TII_MEDIA_TYPE, TII_DIGEST, 1)])))
                    .unwrap()
                    .replace(
                        OCI_IMAGE_MEDIA_TYPE,
                        "application/vnd.oci.image.index.v1+json",
                    )
                    .into_bytes(),
                "media type application/vnd.oci.image.index.v1+json is not an image manifest",
            ),
            (b"[]".to_vec(), "not an image manifest"),
        ];
        for (bytes, expected) in cases {
            let message = inspect(&bytes).unwrap_err().to_string();
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn references_join_the_registry_host_and_the_ref() {
        let (parsed, protocol) = reference(&source(
            "https://oci.tx3.land/",
            "open-tx3/strike-staking:0.1.0",
            b"",
        ))
        .unwrap();
        assert_eq!(parsed.registry(), "oci.tx3.land");
        assert_eq!(parsed.repository(), "open-tx3/strike-staking");
        assert_eq!(parsed.tag(), Some("0.1.0"));
        assert!(matches!(protocol, ClientProtocol::Https));

        let (parsed, protocol) =
            reference(&source("http://127.0.0.1:5000", "acme/transfer:0.1.0", b"")).unwrap();
        assert_eq!(parsed.registry(), "127.0.0.1:5000");
        assert!(matches!(protocol, ClientProtocol::Http));
    }

    #[test]
    fn references_reject_unaddressable_registries() {
        for (url, reference_text, expected) in [
            ("oci.tx3.land", "a/b:1", "must be http:// or https://"),
            (
                "https://oci.tx3.land/v2",
                "a/b:1",
                "only a host and an optional port",
            ),
            (
                "https://oci.tx3.land?x=1",
                "a/b:1",
                "only a host and an optional port",
            ),
            (
                "http://zot",
                "a/b:1",
                "`zot` is not read as a registry host",
            ),
            ("https://oci.tx3.land", "Acme/B:1", "invalid reference"),
        ] {
            let Err(err) = reference(&source(url, reference_text, b"")) else {
                panic!("{url} {reference_text} was accepted");
            };
            let message = err.to_string();
            assert!(
                message.starts_with(&format!("cannot address {reference_text} at {url}: ")),
                "{message}"
            );
            assert!(message.contains(expected), "{message}");
        }
    }
}
