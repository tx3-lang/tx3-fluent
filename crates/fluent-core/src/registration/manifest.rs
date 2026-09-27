//! `registration.toml`: the manifest that binds a protocol, its TII, its
//! consumption skill and its deployment profile.

use std::fmt;
use std::path::{Component, Path};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::config::check_url;

/// File name of the manifest inside every registration bundle.
pub const MANIFEST_FILE: &str = "registration.toml";

/// TII path used by local bundles whose manifest names none.
pub const DEFAULT_TII_PATH: &str = "protocol.tii";

/// Skill path used by bundles whose manifest names none.
pub const DEFAULT_SKILL_PATH: &str = "SKILL.md";

/// A parsed `registration.toml`. Unknown keys are errors.
///
/// Parsing checks the shape; [`Manifest::validate`] checks the rules that do
/// not need the TII or the skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Unique registration identifier, matching `^[a-z][a-z0-9_]{2,40}$`.
    pub slug: String,
    /// `[protocol]`: the protocol identity and where its TII comes from.
    pub protocol: ManifestProtocol,
    /// `[artifact]`: the TII file and its expected digest.
    #[serde(default)]
    pub artifact: ManifestArtifact,
    /// `[skill]`: the consumption skill.
    #[serde(default)]
    pub skill: ManifestSkill,
    /// `[deployment]`: the TII profile and the network it serves.
    pub deployment: Deployment,
}

/// `[protocol]`: the protocol identity, equal to the TII `protocol` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestProtocol {
    /// The registry scope that publishes the protocol.
    pub scope: String,
    /// The protocol name within its scope.
    pub name: String,
    /// The protocol version.
    pub version: String,
    /// Where the TII comes from.
    pub source: ProtocolSource,
    /// `[protocol.registry]`: required when `source = "registry"`, rejected
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<RegistrySource>,
}

/// Where a registration's TII comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolSource {
    /// A TII file inside the bundle.
    Local,
    /// A digest-pinned registry artifact; the bundle carries no TII copy.
    Registry,
}

/// `[protocol.registry]`: the registry artifact a registration is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySource {
    /// Registry base URL, for example `https://oci.tx3.land`.
    pub url: String,
    /// Artifact reference, for example `open-tx3/strike-staking:0.2.0`.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Content digest of the artifact manifest, `sha256:<hex>`.
    pub manifest_digest: String,
}

/// `[artifact]`: the TII file and its expected digest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestArtifact {
    /// TII path relative to the bundle; local bundles only. Defaults to
    /// [`DEFAULT_TII_PATH`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tii: Option<String>,
    /// Expected `sha256:<hex>` of the TII bytes. Required for registry
    /// bundles (the `application/tii+json` layer digest); optional for local
    /// bundles, and checked when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tii_digest: Option<String>,
}

/// `[skill]`: the consumption skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSkill {
    /// Skill path relative to the bundle. Defaults to [`DEFAULT_SKILL_PATH`].
    #[serde(default = "default_skill_path")]
    pub path: String,
}

impl Default for ManifestSkill {
    fn default() -> Self {
        ManifestSkill {
            path: default_skill_path(),
        }
    }
}

fn default_skill_path() -> String {
    DEFAULT_SKILL_PATH.to_string()
}

/// `[deployment]`: the TII profile a registration binds and its network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    /// The TII profile supplying deployment values. Must be `mainnet`,
    /// `preprod` or `preview`.
    pub profile: String,
    /// The network the profile serves; must equal the profile's network.
    pub network: Network,
}

/// A Cardano network a registration can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// Cardano mainnet.
    Mainnet,
    /// The preprod testnet.
    Preprod,
    /// The preview testnet.
    Preview,
}

impl Network {
    /// Every network, in declaration order.
    pub const ALL: [Network; 3] = [Network::Mainnet, Network::Preprod, Network::Preview];

    /// The network's wire name, identical to its serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Preprod => "preprod",
            Network::Preview => "preview",
        }
    }

    /// The network a deployment profile serves, or `None` for profiles that
    /// cannot be deployed (such as `local`).
    pub fn from_profile(profile: &str) -> Option<Network> {
        profile.parse().ok()
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Network::ALL
            .into_iter()
            .find(|n| n.as_str() == s)
            .ok_or_else(|| format!("unknown network `{s}`"))
    }
}

impl Manifest {
    /// Parses manifest TOML. Unknown keys are errors.
    pub fn from_toml_str(text: &str) -> Result<Manifest, toml::de::Error> {
        toml::from_str(text)
    }

    /// Checks the rules that do not need the TII or the skill, returning every
    /// problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();

        if !is_slug(&self.slug) {
            problems.push(format!(
                "slug `{}` must match ^[a-z][a-z0-9_]{{2,40}}$",
                self.slug
            ));
        }
        for (key, value) in [
            ("protocol.scope", &self.protocol.scope),
            ("protocol.name", &self.protocol.name),
            ("protocol.version", &self.protocol.version),
        ] {
            if value.is_empty() {
                problems.push(format!("{key} must not be empty"));
            }
        }

        match (self.protocol.source, &self.protocol.registry) {
            (ProtocolSource::Local, Some(_)) => problems
                .push("[protocol.registry] is only allowed when source = \"registry\"".to_string()),
            (ProtocolSource::Registry, None) => problems
                .push("[protocol.registry] is required when source = \"registry\"".to_string()),
            (ProtocolSource::Registry, Some(registry)) => {
                check_url(&mut problems, "protocol.registry.url", &registry.url);
                if registry.reference.is_empty() {
                    problems.push("protocol.registry.ref must not be empty".to_string());
                }
                check_digest(
                    &mut problems,
                    "protocol.registry.manifest_digest",
                    &registry.manifest_digest,
                );
            }
            (ProtocolSource::Local, None) => {}
        }

        if self.protocol.source == ProtocolSource::Registry {
            if self.artifact.tii.is_some() {
                problems.push(
                    "artifact.tii is not allowed when source = \"registry\": \
                     the TII comes from the registry"
                        .to_string(),
                );
            }
            if self.artifact.tii_digest.is_none() {
                problems
                    .push("artifact.tii_digest is required when source = \"registry\"".to_string());
            }
        }
        if let Some(digest) = &self.artifact.tii_digest {
            check_digest(&mut problems, "artifact.tii_digest", digest);
        }
        if let Some(tii) = &self.artifact.tii {
            check_bundle_path(&mut problems, "artifact.tii", tii);
        }
        check_bundle_path(&mut problems, "skill.path", &self.skill.path);

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// The protocol identity as `scope/name:version`, the form skills bind to.
    pub fn protocol_id(&self) -> String {
        let p = &self.protocol;
        format!("{}/{}:{}", p.scope, p.name, p.version)
    }

    /// The TII path relative to the bundle, for local bundles.
    pub fn tii_path(&self) -> &str {
        self.artifact.tii.as_deref().unwrap_or(DEFAULT_TII_PATH)
    }
}

/// Whether `slug` matches `^[a-z][a-z0-9_]{2,40}$`.
pub fn is_slug(slug: &str) -> bool {
    let mut bytes = slug.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_lowercase())
        && (3..=41).contains(&slug.len())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `digest` is `sha256:` followed by 64 lowercase hex digits.
pub fn is_sha256_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn check_digest(problems: &mut Vec<String>, key: &str, digest: &str) {
    if !is_sha256_digest(digest) {
        problems.push(format!(
            "{key} must be `sha256:` followed by 64 lowercase hex digits"
        ));
    }
}

/// Bundle files are named by relative paths that stay inside the bundle.
fn check_bundle_path(problems: &mut Vec<String>, key: &str, path: &str) {
    let components: Vec<Component> = Path::new(path).components().collect();
    let inside = components
        .iter()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && components.iter().any(|c| matches!(c, Component::Normal(_)));
    if !inside {
        problems.push(format!(
            "{key} must be a relative path inside the bundle, without `..`"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = r#"
        slug = "transfer_preprod"
        [protocol]
        scope = "acme"
        name = "transfer"
        version = "0.1.0"
        source = "local"
        [deployment]
        profile = "preprod"
        network = "preprod"
    "#;

    const REGISTRY: &str = r#"
        slug = "strike_staking_mainnet"
        [protocol]
        scope = "open-tx3"
        name = "strike-staking"
        version = "0.2.0"
        source = "registry"
        [protocol.registry]
        url = "https://oci.tx3.land"
        ref = "open-tx3/strike-staking:0.2.0"
        manifest_digest = "sha256:0c9bedc763b421cbec421da2a8becfcdb2058501b25bfdbfb4d0db57a29a3dc0"
        [artifact]
        tii_digest = "sha256:de15465a47728deb3483650e628301e9d19e9c91a584626d4672525bfcba5ca1"
        [skill]
        path = "SKILL.md"
        [deployment]
        profile = "mainnet"
        network = "mainnet"
    "#;

    fn problems(text: &str) -> Vec<String> {
        Manifest::from_toml_str(text)
            .unwrap()
            .validate()
            .unwrap_err()
    }

    #[test]
    fn local_manifest_applies_defaults() {
        let manifest = Manifest::from_toml_str(LOCAL).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.tii_path(), "protocol.tii");
        assert_eq!(manifest.skill.path, "SKILL.md");
        assert_eq!(manifest.artifact.tii_digest, None);
        assert_eq!(manifest.protocol_id(), "acme/transfer:0.1.0");
        assert_eq!(manifest.deployment.network, Network::Preprod);
    }

    #[test]
    fn registry_manifest_validates() {
        let manifest = Manifest::from_toml_str(REGISTRY).unwrap();
        manifest.validate().unwrap();
        let registry = manifest.protocol.registry.unwrap();
        assert_eq!(registry.reference, "open-tx3/strike-staking:0.2.0");
    }

    #[test]
    fn rejects_unknown_keys_and_networks() {
        for text in [
            format!("{LOCAL}\nsurprise = 1"),
            LOCAL.replace(
                "source = \"local\"",
                "source = \"local\"\nlicense = \"MIT\"",
            ),
            LOCAL.replace("network = \"preprod\"", "network = \"devnet\""),
            LOCAL.replace("source = \"local\"", "source = \"git\""),
        ] {
            assert!(Manifest::from_toml_str(&text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn slug_rule() {
        for ok in [
            "abc",
            "a_1",
            "transfer_preprod",
            &format!("a{}", "b".repeat(40)),
        ] {
            assert!(is_slug(ok), "{ok}");
        }
        for bad in [
            "",
            "ab",
            "1abc",
            "_abc",
            "Abc",
            "a-bc",
            "abc ",
            &format!("a{}", "b".repeat(41)),
        ] {
            assert!(!is_slug(bad), "{bad}");
        }
    }

    #[test]
    fn registry_source_requires_its_table_and_digest_and_no_tii() {
        // Swap the digest for a TII path.
        let text = REGISTRY.replace("tii_digest", "tii = \"protocol.tii\"\n#");
        assert_eq!(
            problems(&text),
            [
                "artifact.tii is not allowed when source = \"registry\": \
                 the TII comes from the registry",
                "artifact.tii_digest is required when source = \"registry\"",
            ]
        );

        let start = REGISTRY.find("[protocol.registry]").unwrap();
        let end = REGISTRY.find("[artifact]").unwrap();
        let text = format!("{}{}", &REGISTRY[..start], &REGISTRY[end..]);
        assert_eq!(
            problems(&text),
            ["[protocol.registry] is required when source = \"registry\""]
        );
    }

    #[test]
    fn local_source_rejects_a_registry_table() {
        let text = REGISTRY.replace("source = \"registry\"", "source = \"local\"");
        assert_eq!(
            problems(&text),
            ["[protocol.registry] is only allowed when source = \"registry\""]
        );
    }

    #[test]
    fn validates_digests_urls_and_paths() {
        let text = REGISTRY
            .replace("sha256:0c9b", "sha256:0C9B")
            .replace("sha256:de15465a47728deb", "md5:de15465a47728deb")
            .replace("https://oci.tx3.land", "oci.tx3.land")
            .replace("path = \"SKILL.md\"", "path = \"../SKILL.md\"")
            .replace("scope = \"open-tx3\"", "scope = \"\"");
        assert_eq!(
            problems(&text),
            [
                "protocol.scope must not be empty",
                "protocol.registry.url must be an http:// or https:// URL",
                "protocol.registry.manifest_digest must be `sha256:` followed by 64 lowercase \
                 hex digits",
                "artifact.tii_digest must be `sha256:` followed by 64 lowercase hex digits",
                "skill.path must be a relative path inside the bundle, without `..`",
            ]
        );
    }

    #[test]
    fn bundle_paths_stay_inside_the_bundle() {
        for ok in ["protocol.tii", "./protocol.tii", "tii/protocol.tii"] {
            let mut found = Vec::new();
            check_bundle_path(&mut found, "k", ok);
            assert!(found.is_empty(), "{ok}");
        }
        for bad in ["", ".", "/etc/passwd", "../x.tii", "tii/../../x.tii"] {
            let mut found = Vec::new();
            check_bundle_path(&mut found, "k", bad);
            assert_eq!(found.len(), 1, "{bad}");
        }
    }

    #[test]
    fn networks_derive_from_profile_names_only() {
        assert_eq!(Network::from_profile("mainnet"), Some(Network::Mainnet));
        assert_eq!(Network::from_profile("preprod"), Some(Network::Preprod));
        assert_eq!(Network::from_profile("preview"), Some(Network::Preview));
        for other in ["local", "devnet", "Mainnet", ""] {
            assert_eq!(Network::from_profile(other), None, "{other}");
        }
    }
}
