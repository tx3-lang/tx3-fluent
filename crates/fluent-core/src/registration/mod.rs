//! Protocol registrations: bundles on disk, loaded once into an immutable
//! [`Catalog`].
//!
//! `[registrations].dir` holds one subdirectory per registration. Each bundle
//! contains a [`Manifest`] (`registration.toml`), a [`SkillDocument`]
//! (`SKILL.md` unless the manifest names another path) and, for
//! `source = "local"` only, the TII (`protocol.tii` unless the manifest names
//! another path). Registry-sourced bundles carry no TII copy: their TII is
//! fetched with an [`OciFetcher`] and cached in [`CACHE_DIR`]; see
//! [`crate::registry`]. Entries whose names start with `.` and plain files are
//! skipped.
//!
//! [`load_dir`] loads every bundle, rejecting each one that breaks a rule with
//! a [`FluentError`] whose message names the bundle and the rule. Registrations
//! are loaded once; changing a bundle takes effect on restart.
//!
//! # Digests and revision
//!
//! `tii_digest` and `skill_digest` are `sha256:<hex>` over the exact file
//! bytes. The revision is the first 12 hex digits of the SHA-256 of the UTF-8
//! string `{tii_digest}{skill_digest}{profile}{network}`, with the digests in
//! their `sha256:<hex>` form; see [`revision`].

mod manifest;
mod skill;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tx3_sdk::tii::Protocol;
use tx3_sdk::tii::spec::TiiFile;

use crate::error::FluentError;
use crate::registry::{CACHE_DIR, OciFetcher, Provenance};

pub use manifest::{
    DEFAULT_SKILL_PATH, DEFAULT_TII_PATH, Deployment, MANIFEST_FILE, Manifest, ManifestArtifact,
    ManifestProtocol, ManifestSkill, Network, ProtocolSource, RegistrySource, is_sha256_digest,
    is_slug,
};
pub use skill::{SkillDependency, SkillDocument};

/// A validated registration: a protocol's TII bound to its consumption skill
/// and one deployment profile. Immutable once loaded.
#[derive(Debug, Clone)]
pub struct Registration {
    bundle: PathBuf,
    manifest: Manifest,
    tii: TiiFile,
    protocol: Protocol,
    skill: SkillDocument,
    network: Network,
    tii_digest: String,
    skill_digest: String,
    revision: String,
    provenance: Option<Provenance>,
}

impl Registration {
    /// Loads and validates the bundle in `bundle`. A registry-sourced TII is
    /// fetched with the default [`OciFetcher`], caching in the [`CACHE_DIR`]
    /// beside the bundle.
    ///
    /// Every failure is reported with the bundle path: a broken network rule
    /// as [`FluentError::NetworkMismatch`], every other rule, including a
    /// failed fetch, as [`FluentError::RegistrationUnavailable`].
    pub async fn load(bundle: impl AsRef<Path>) -> Result<Registration, FluentError> {
        let bundle = bundle.as_ref();
        let parent = bundle.parent().unwrap_or(Path::new(""));
        Registration::load_with(bundle, &OciFetcher::new(parent.join(CACHE_DIR))).await
    }

    async fn load_with(bundle: &Path, fetcher: &OciFetcher) -> Result<Registration, FluentError> {
        let reject = |reason: String| FluentError::RegistrationUnavailable {
            registration: bundle.display().to_string(),
            reason,
        };

        let manifest_text = fs::read_to_string(bundle.join(MANIFEST_FILE))
            .map_err(|err| reject(format!("cannot read {MANIFEST_FILE}: {err}")))?;
        let manifest = Manifest::from_toml_str(&manifest_text).map_err(|err| {
            let line = err
                .span()
                .and_then(|span| manifest_text.get(..span.start))
                .map(|before| format!("line {}: ", before.matches('\n').count() + 1))
                .unwrap_or_default();
            reject(format!(
                "invalid {MANIFEST_FILE}: {line}{}",
                err.message().trim_end()
            ))
        })?;
        manifest.validate().map_err(|problems| {
            reject(format!("invalid {MANIFEST_FILE}: {}", problems.join("; ")))
        })?;

        let (tii_bytes, provenance) = match (manifest.protocol.source, &manifest.protocol.registry)
        {
            (ProtocolSource::Local, _) => {
                let path = manifest.tii_path();
                let bytes = fs::read(bundle.join(path))
                    .map_err(|err| reject(format!("cannot read TII {path}: {err}")))?;
                (bytes, None)
            }
            (ProtocolSource::Registry, registry) => {
                if bundle.join(DEFAULT_TII_PATH).exists() {
                    return Err(reject(format!(
                        "a registry-sourced bundle must not carry {DEFAULT_TII_PATH}: \
                         its TII comes only from the registry"
                    )));
                }
                // `Manifest::validate` requires the table for this source.
                let Some(registry) = registry else {
                    return Err(reject(
                        "[protocol.registry] is required when source = \"registry\"".to_string(),
                    ));
                };
                let fetched = fetcher
                    .fetch_tii(registry)
                    .await
                    .map_err(|err| reject(err.to_string()))?;
                (fetched.tii, Some(fetched.provenance))
            }
        };

        let tii_digest = sha256_digest(&tii_bytes);
        if let Some(expected) = &manifest.artifact.tii_digest
            && *expected != tii_digest
        {
            return Err(reject(format!(
                "TII digest mismatch: {MANIFEST_FILE} declares {expected}, the TII is {tii_digest}"
            )));
        }

        let tii_text = String::from_utf8(tii_bytes)
            .map_err(|_| reject("malformed TII: not UTF-8".to_string()))?;
        // The spec model first: its errors carry a line and column.
        let tii: TiiFile = serde_json::from_str(&tii_text)
            .map_err(|err| reject(format!("malformed TII: {err}")))?;
        let protocol = Protocol::from_string(tii_text)
            .map_err(|err| reject(format!("malformed TII: {err}")))?;

        let declared = &tii.protocol;
        let expected = &manifest.protocol;
        if (&declared.scope, &declared.name, &declared.version)
            != (&expected.scope, &expected.name, &expected.version)
        {
            return Err(reject(format!(
                "protocol mismatch: {MANIFEST_FILE} declares {}, the TII declares {}/{}:{}",
                manifest.protocol_id(),
                declared.scope,
                declared.name,
                declared.version
            )));
        }

        let deployment = &manifest.deployment;
        if !tii.profiles.contains_key(&deployment.profile) {
            let mut available: Vec<&str> = tii.profiles.keys().map(String::as_str).collect();
            available.sort_unstable();
            return Err(reject(format!(
                "missing profile: `{}` is not defined in the TII (defined: {})",
                deployment.profile,
                available.join(", ")
            )));
        }
        let network = Network::from_profile(&deployment.profile).ok_or_else(|| {
            reject(format!(
                "profile `{}` cannot be deployed: only the mainnet, preprod and preview \
                 profiles are allowed",
                deployment.profile
            ))
        })?;
        if network != deployment.network {
            return Err(FluentError::NetworkMismatch {
                requested: deployment.network.to_string(),
                available: vec![network.to_string()],
                registration: Some(bundle.display().to_string()),
            });
        }

        let skill_path = &manifest.skill.path;
        let skill_bytes = fs::read(bundle.join(skill_path))
            .map_err(|err| reject(format!("cannot read skill {skill_path}: {err}")))?;
        let skill_digest = sha256_digest(&skill_bytes);
        let skill_text = String::from_utf8(skill_bytes)
            .map_err(|_| reject(format!("invalid skill {skill_path}: not UTF-8")))?;
        let skill = SkillDocument::parse(&skill_text)
            .map_err(|err| reject(format!("invalid skill {skill_path}: {err}")))?;
        check_skill_binding(&skill, &manifest, &tii, &tii_digest)
            .map_err(|problems| reject(format!("incompatible skill binding: {problems}")))?;

        let revision = revision(&tii_digest, &skill_digest, &deployment.profile, network);
        Ok(Registration {
            bundle: bundle.to_path_buf(),
            manifest,
            tii,
            protocol,
            skill,
            network,
            tii_digest,
            skill_digest,
            revision,
            provenance,
        })
    }

    /// The registration's unique identifier.
    pub fn slug(&self) -> &str {
        &self.manifest.slug
    }

    /// The bundle directory it was loaded from.
    pub fn bundle(&self) -> &Path {
        &self.bundle
    }

    /// The manifest, as written.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The protocol identity as `scope/name:version`.
    pub fn protocol_id(&self) -> String {
        self.manifest.protocol_id()
    }

    /// The TII, for metadata: transactions, parameters, parties and profiles.
    pub fn tii(&self) -> &TiiFile {
        &self.tii
    }

    /// The TII loaded by the SDK, for invoking transactions.
    pub fn protocol(&self) -> &Protocol {
        &self.protocol
    }

    /// The consumption skill.
    pub fn skill(&self) -> &SkillDocument {
        &self.skill
    }

    /// The deployment profile.
    pub fn profile(&self) -> &str {
        &self.manifest.deployment.profile
    }

    /// The network the registration serves.
    pub fn network(&self) -> Network {
        self.network
    }

    /// `sha256:<hex>` of the TII bytes.
    pub fn tii_digest(&self) -> &str {
        &self.tii_digest
    }

    /// `sha256:<hex>` of the skill bytes.
    pub fn skill_digest(&self) -> &str {
        &self.skill_digest
    }

    /// The registration revision; see [`revision`].
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Where a registry-sourced TII came from; `None` for local bundles.
    /// Recorded, not verified: it does not identify the publisher.
    pub fn provenance(&self) -> Option<&Provenance> {
        self.provenance.as_ref()
    }
}

/// Every problem between a skill's frontmatter and its registration, joined.
fn check_skill_binding(
    skill: &SkillDocument,
    manifest: &Manifest,
    tii: &TiiFile,
    tii_digest: &str,
) -> Result<(), String> {
    let mut problems = Vec::new();
    let protocol_id = manifest.protocol_id();
    if skill.protocol != protocol_id {
        problems.push(format!(
            "skill protocol is {}, the registration is {protocol_id}",
            skill.protocol
        ));
    }
    if skill.tii_digest != tii_digest {
        problems.push(format!(
            "skill tii_digest is {}, the TII is {tii_digest}",
            skill.tii_digest
        ));
    }
    if skill.network != manifest.deployment.network {
        problems.push(format!(
            "skill network is {}, the registration serves {}",
            skill.network, manifest.deployment.network
        ));
    }
    for dependency in &skill.dependencies {
        for tx in &dependency.required_for {
            if !tii.transactions.contains_key(tx) {
                problems.push(format!(
                    "skill dependency `{}` is required for `{tx}`, which the TII does not define",
                    dependency.id
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// `sha256:<hex>` of `bytes`.
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// The registration revision: the first 12 hex digits of the SHA-256 of
/// `{tii_digest}{skill_digest}{profile}{network}`, concatenated as UTF-8 with
/// the digests in their `sha256:<hex>` form.
pub fn revision(tii_digest: &str, skill_digest: &str, profile: &str, network: Network) -> String {
    let mut hasher = Sha256::new();
    for part in [tii_digest, skill_digest, profile, network.as_str()] {
        hasher.update(part.as_bytes());
    }
    let mut hex = hex::encode(hasher.finalize());
    hex.truncate(12);
    hex
}

/// Loaded registrations by slug, in slug order. Immutable once built.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    registrations: BTreeMap<String, Arc<Registration>>,
}

impl Catalog {
    /// The registration with `slug`, if one was loaded.
    pub fn get(&self, slug: &str) -> Option<&Arc<Registration>> {
        self.registrations.get(slug)
    }

    /// Every registration, in slug order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Registration>> {
        self.registrations.values()
    }

    /// The number of registrations.
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Whether no registration was loaded.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }
}

/// A bundle [`load_dir`] rejected.
#[derive(Debug)]
pub struct Rejection {
    /// The bundle directory.
    pub bundle: PathBuf,
    /// Why it was rejected; the message names the bundle.
    pub error: FluentError,
}

/// The outcome of [`load_dir`]: what loaded and what was rejected.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Every registration that passed every rule.
    pub catalog: Catalog,
    /// Every rejected bundle, in bundle path order.
    pub rejected: Vec<Rejection>,
}

/// Loads every bundle in `dir`, fetching registry-sourced TII with the
/// default [`OciFetcher`] and caching it in `dir`/[`CACHE_DIR`].
///
/// A bundle that breaks a rule, or whose TII cannot be fetched, is rejected
/// on its own and logged; the others still load. Slugs must be unique across
/// the directory: every bundle claiming a slug that another loaded bundle also
/// claims is rejected, so neither is served. Fails only when `dir` itself
/// cannot be read.
pub async fn load_dir(dir: impl AsRef<Path>) -> Result<Loaded, FluentError> {
    let dir = dir.as_ref();
    load_dir_with(dir, &OciFetcher::new(dir.join(CACHE_DIR))).await
}

/// [`load_dir`] with a given fetcher, for example one with another timeout.
pub async fn load_dir_with(
    dir: impl AsRef<Path>,
    fetcher: &OciFetcher,
) -> Result<Loaded, FluentError> {
    let dir = dir.as_ref();
    let unreadable = |err: std::io::Error| FluentError::RegistrationUnavailable {
        registration: dir.display().to_string(),
        reason: format!("cannot read the registrations directory: {err}"),
    };

    let mut bundles = Vec::new();
    for entry in fs::read_dir(dir).map_err(unreadable)? {
        let path = entry.map_err(unreadable)?.path();
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if !hidden && path.is_dir() {
            bundles.push(path);
        }
    }
    bundles.sort();

    let mut loaded: BTreeMap<String, Vec<Registration>> = BTreeMap::new();
    let mut rejected = Vec::new();
    for bundle in bundles {
        match Registration::load_with(&bundle, fetcher).await {
            Ok(registration) => loaded
                .entry(registration.slug().to_string())
                .or_default()
                .push(registration),
            Err(error) => rejected.push(Rejection { bundle, error }),
        }
    }

    let mut registrations = BTreeMap::new();
    for (slug, mut claimants) in loaded {
        if claimants.len() == 1 {
            if let Some(registration) = claimants.pop() {
                registrations.insert(slug, Arc::new(registration));
            }
            continue;
        }
        for registration in &claimants {
            let others: Vec<String> = claimants
                .iter()
                .filter(|other| other.bundle != registration.bundle)
                .map(|other| other.bundle.display().to_string())
                .collect();
            rejected.push(Rejection {
                bundle: registration.bundle.clone(),
                error: FluentError::RegistrationUnavailable {
                    registration: registration.bundle.display().to_string(),
                    reason: format!(
                        "duplicate slug `{slug}`, also used by {}",
                        others.join(", ")
                    ),
                },
            });
        }
    }
    rejected.sort_by(|a, b| a.bundle.cmp(&b.bundle));
    for rejection in &rejected {
        tracing::warn!(
            bundle = %rejection.bundle.display(),
            code = rejection.error.code().as_str(),
            "{}",
            rejection.error.message()
        );
    }

    Ok(Loaded {
        catalog: Catalog { registrations },
        rejected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_prefixed_sha256_hex() {
        // printf 'abc' | sha256sum
        assert_eq!(
            sha256_digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_digest(&sha256_digest(b"")));
    }

    #[test]
    fn revision_hashes_the_concatenated_parts() {
        let tii = sha256_digest(b"tii");
        let skill = sha256_digest(b"skill");
        let joined = format!("{tii}{skill}preprodpreprod");
        let full = sha256_digest(joined.as_bytes());
        let hex = full.strip_prefix("sha256:").unwrap();
        assert_eq!(
            revision(&tii, &skill, "preprod", Network::Preprod),
            hex[..12]
        );
        assert_ne!(
            revision(&tii, &skill, "preprod", Network::Preprod),
            revision(&skill, &tii, "preprod", Network::Preprod)
        );
    }
}
