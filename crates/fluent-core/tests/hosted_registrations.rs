//! The hosted registrations in `deploy/hosted/registrations/`: every bundle
//! loads against its pinned registry artifact, served offline from
//! `tests/fixtures/registry/{repository}/{tag}/`, and every skill follows
//! `docs/skill-template.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fluent_core::registration::{Manifest, SkillDocument, load_dir, sha256_digest};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The hosted bundles, by slug; each is also its directory name.
const HOSTED: [&str; 1] = ["transfer_preprod"];

/// The registry every hosted bundle is pinned to.
const REGISTRY_URL: &str = "https://oci.tx3.land";

const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// The level-two headings of a skill body, in order.
const SECTIONS: [&str; 7] = [
    "Purpose",
    "Before you start",
    "Units",
    "Argument preparation",
    "External dependencies",
    "After preparation",
    "Do not",
];

fn hosted_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/hosted/registrations")
}

/// The published artifact a manifest is pinned to, as stored in the fixtures.
struct Artifact {
    repository: String,
    tag: String,
    manifest: Vec<u8>,
    tii: Vec<u8>,
}

impl Artifact {
    fn pinned_by(manifest: &Manifest) -> Artifact {
        let registry = manifest
            .protocol
            .registry
            .as_ref()
            .expect("registry source");
        let (repository, tag) = registry
            .reference
            .rsplit_once(':')
            .expect("ref is repository:tag");
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/registry")
            .join(repository)
            .join(tag);
        let read = |name: &str| {
            fs::read(dir.join(name)).unwrap_or_else(|err| panic!("{}/{name}: {err}", dir.display()))
        };
        Artifact {
            repository: repository.to_string(),
            tag: tag.to_string(),
            manifest: read("manifest.json"),
            tii: read("protocol.tii"),
        }
    }

    /// The TII transaction names.
    fn transactions(&self) -> BTreeSet<String> {
        let tii: Value = serde_json::from_slice(&self.tii).expect("TII is JSON");
        tii["transactions"]
            .as_object()
            .expect("TII transactions")
            .keys()
            .cloned()
            .collect()
    }
}

/// Every hosted bundle's manifest, by directory name; hidden entries and plain
/// files are skipped, as the loader does.
fn bundles() -> Vec<(String, Manifest)> {
    let mut bundles = Vec::new();
    for entry in fs::read_dir(hosted_dir()).expect("read deploy/hosted/registrations") {
        let path = entry.expect("directory entry").path();
        let name = path
            .file_name()
            .expect("bundle name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with('.') || !path.is_dir() {
            continue;
        }
        let text = fs::read_to_string(path.join("registration.toml")).expect("read manifest");
        let manifest = Manifest::from_toml_str(&text).expect("parse manifest");
        bundles.push((name, manifest));
    }
    bundles.sort_by(|a, b| a.0.cmp(&b.0));
    bundles
}

fn skill(slug: &str) -> SkillDocument {
    let text = fs::read_to_string(hosted_dir().join(slug).join("SKILL.md")).expect("read skill");
    SkillDocument::parse(&text).unwrap_or_else(|err| panic!("{slug}: {err}"))
}

/// A fresh, empty registrations directory under the test target directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("hosted")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clear scratch directory");
    }
    fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Copies the bundle `name` into `dir`, pointing its registry URL at `url`.
fn copy_bundle(name: &str, dir: &Path, url: &str) {
    let source = hosted_dir().join(name);
    let target = dir.join(name);
    fs::create_dir_all(&target).expect("create bundle directory");
    for entry in fs::read_dir(&source).expect("read bundle") {
        let path = entry.expect("bundle entry").path();
        let file = path.file_name().expect("file name");
        if file == "registration.toml" {
            let text = fs::read_to_string(&path).expect("read manifest");
            let line = format!("url = \"{REGISTRY_URL}\"");
            assert_eq!(text.matches(&line).count(), 1, "{name}: {line}");
            let text = text.replace(&line, &format!("url = \"{url}\""));
            fs::write(target.join(file), text).expect("write manifest");
        } else {
            fs::copy(&path, target.join(file)).expect("copy bundle file");
        }
    }
}

#[test]
fn the_hosted_bundles_are_the_reviewed_ones() {
    let names: Vec<String> = bundles().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, HOSTED);
}

#[tokio::test]
async fn every_hosted_bundle_loads_against_its_pinned_artifact() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let dir = scratch("load");
    let bundles = bundles();
    for (name, manifest) in &bundles {
        let registry = manifest
            .protocol
            .registry
            .as_ref()
            .expect("registry source");
        assert_eq!(registry.url, REGISTRY_URL, "{name}");
        assert_eq!(registry.reference, manifest.protocol_id(), "{name}");

        let artifact = Artifact::pinned_by(manifest);
        assert_eq!(
            sha256_digest(&artifact.manifest),
            registry.manifest_digest,
            "{name}: the fixture is not the pinned manifest"
        );
        Mock::given(method("GET"))
            .and(path(format!(
                "/v2/{}/manifests/{}",
                artifact.repository, artifact.tag
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(artifact.manifest, MANIFEST_MEDIA_TYPE),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v2/{}/blobs/{}",
                artifact.repository,
                sha256_digest(&artifact.tii)
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(artifact.tii, "application/octet-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        copy_bundle(name, &dir, &server.uri());
    }

    let loaded = load_dir(&dir).await.expect("load the hosted bundles");
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    let slugs: Vec<&str> = loaded.catalog.iter().map(|r| r.slug()).collect();
    assert_eq!(slugs, HOSTED);

    for (name, manifest) in &bundles {
        let registration = loaded.catalog.get(&manifest.slug).expect("loaded");
        assert_eq!(registration.slug(), name);
        let provenance = registration.provenance().expect("registry provenance");
        let registry = manifest.protocol.registry.as_ref().unwrap();
        assert_eq!(provenance.manifest_digest, registry.manifest_digest);
        assert_eq!(
            Some(registration.tii_digest()),
            manifest.artifact.tii_digest.as_deref()
        );

        let skill = registration.skill();
        assert_eq!(skill.name, name.replace('_', "-"));
        assert_eq!(skill.protocol, registration.protocol_id());
        assert_eq!(skill.tii_digest, registration.tii_digest());
        assert_eq!(skill.network, registration.network());
    }
}

/// The level-two and level-three headings of a Markdown body, outside fenced
/// code blocks, as `(level, text)`.
fn headings(body: &str) -> Vec<(usize, &str)> {
    let mut fenced = false;
    let mut found = Vec::new();
    for line in body.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            if let Some(text) = line.strip_prefix("## ") {
                found.push((2, text.trim()));
            } else if let Some(text) = line.strip_prefix("### ") {
                found.push((3, text.trim()));
            }
        }
    }
    found
}

#[test]
fn every_skill_follows_the_template() {
    for (name, manifest) in bundles() {
        let skill = skill(&name);
        assert_eq!(skill.license.as_deref(), Some("Apache-2.0"), "{name}");
        assert!(
            skill.description.contains("; use when "),
            "{name}: the description must end by saying when to use the skill"
        );
        assert!(
            skill.body.starts_with("# "),
            "{name}: the body must start with a title"
        );

        let headings = headings(&skill.body);
        let sections: Vec<&str> = headings
            .iter()
            .filter(|(level, _)| *level == 2)
            .map(|(_, text)| *text)
            .collect();
        assert_eq!(sections, SECTIONS, "{name}: sections");

        // Level-three headings inside Argument preparation name transactions.
        let mut in_arguments = false;
        let mut prepared = BTreeSet::new();
        for (level, text) in &headings {
            if *level == 2 {
                in_arguments = *text == "Argument preparation";
            } else if in_arguments {
                let tx = text
                    .strip_prefix('`')
                    .and_then(|t| t.strip_suffix('`'))
                    .unwrap_or_else(|| panic!("{name}: `{text}` is not a transaction name"));
                assert!(prepared.insert(tx.to_string()), "{name}: `{tx}` twice");
            }
        }
        let transactions = Artifact::pinned_by(&manifest).transactions();
        assert_eq!(
            prepared, transactions,
            "{name}: one argument-preparation subsection per TII transaction"
        );

        let dependencies = skill
            .body
            .split("## External dependencies")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .unwrap_or_default();
        for dependency in &skill.dependencies {
            assert!(
                dependencies.contains(&format!("`{}`", dependency.id)),
                "{name}: External dependencies does not explain `{}`",
                dependency.id
            );
        }
    }
}
