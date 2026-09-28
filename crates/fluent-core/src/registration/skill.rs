//! `SKILL.md`: the consumption skill bound to a registration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::manifest::Network;

/// A consumption skill: YAML frontmatter followed by a Markdown body.
///
/// The frontmatter binds the skill to one protocol version, TII digest and
/// network; the loader rejects a skill whose binding does not match its
/// registration. Unknown frontmatter keys are errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillDocument {
    /// The skill name.
    pub name: String,
    /// One sentence saying what the skill covers and when to use it.
    pub description: String,
    /// The skill's license, for example `Apache-2.0`.
    pub license: Option<String>,
    /// The protocol the skill explains, as `scope/name:version`.
    pub protocol: String,
    /// The `sha256:<hex>` digest of the TII the skill was written against.
    pub tii_digest: String,
    /// The network the skill's guidance applies to.
    pub network: Network,
    /// The skill's own revision, starting at 1.
    pub revision: u32,
    /// What the assistant must obtain outside Fluent.
    pub dependencies: Vec<SkillDependency>,
    /// The Markdown after the frontmatter, byte for byte.
    pub body: String,
}

/// Something a skill needs from outside Fluent, and the transactions that need
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillDependency {
    /// Stable identifier of the dependency.
    pub id: String,
    /// What the dependency is and how to obtain it.
    pub description: String,
    /// Names of the transactions that need it.
    pub required_for: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    protocol: String,
    tii_digest: String,
    network: Network,
    revision: u32,
    #[serde(default)]
    dependencies: Vec<SkillDependency>,
}

impl SkillDocument {
    /// Parses a skill: a `---` line, YAML frontmatter, a closing `---` line,
    /// then the Markdown body, which is kept verbatim.
    pub fn parse(text: &str) -> Result<SkillDocument, String> {
        let (yaml, body) = split_frontmatter(text).ok_or_else(|| {
            "must start with YAML frontmatter between two `---` lines".to_string()
        })?;
        let front: Frontmatter = serde_saphyr::from_str(yaml).map_err(|err| {
            let message = err.without_snippet().to_string();
            format!("invalid frontmatter: {}", one_line(&message))
        })?;

        let mut problems = Vec::new();
        for (key, value) in [("name", &front.name), ("description", &front.description)] {
            if value.trim().is_empty() {
                problems.push(format!("frontmatter `{key}` must not be empty"));
            }
        }
        if front.revision == 0 {
            problems.push("frontmatter `revision` starts at 1".to_string());
        }
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }

        Ok(SkillDocument {
            name: front.name,
            description: front.description,
            license: front.license,
            protocol: front.protocol,
            tii_digest: front.tii_digest,
            network: front.network,
            revision: front.revision,
            dependencies: front.dependencies,
            body: body.to_string(),
        })
    }
}

/// Splits `text` into its frontmatter and body, or `None` when it has no
/// frontmatter.
///
/// The frontmatter keeps its opening `---`, a YAML document marker, so parser
/// line numbers match the file.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let opening = ["---\n", "---\r\n"]
        .into_iter()
        .find(|marker| text.starts_with(marker))?;
    let mut offset = opening.len();
    for line in text[offset..].split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((&text[..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// Collapses a possibly multi-line parser message onto one line.
fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\n\
        name: transfer\n\
        description: Move ADA between two addresses. Use when the user asks to send ADA.\n\
        license: Apache-2.0\n\
        protocol: acme/transfer:0.1.0\n\
        tii_digest: sha256:fcc527ea7165969f472870772b64cdbe9b4ad59ad27231790bc484de79983014\n\
        network: preprod\n\
        revision: 2\n\
        dependencies:\n\
        \x20 - id: sender_address\n\
        \x20   description: The user's bech32 address.\n\
        \x20   required_for: [transfer]\n\
        ---\n\
        # Transfer\n\
        \n\
        Body with --- inside a line.\n";

    #[test]
    fn parses_frontmatter_and_keeps_the_body_verbatim() {
        let skill = SkillDocument::parse(SKILL).unwrap();
        assert_eq!(skill.name, "transfer");
        assert_eq!(skill.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(skill.protocol, "acme/transfer:0.1.0");
        assert_eq!(skill.network, Network::Preprod);
        assert_eq!(skill.revision, 2);
        assert_eq!(
            skill.dependencies,
            [SkillDependency {
                id: "sender_address".into(),
                description: "The user's bech32 address.".into(),
                required_for: vec!["transfer".into()],
            }]
        );
        assert_eq!(skill.body, "# Transfer\n\nBody with --- inside a line.\n");
    }

    #[test]
    fn dependencies_and_license_are_optional() {
        let text = SKILL
            .replace("license: Apache-2.0\n", "")
            .replace(
                "dependencies:\n  - id: sender_address\n    description: The user's bech32 address.\n    required_for: [transfer]\n",
                "",
            );
        let skill = SkillDocument::parse(&text).unwrap();
        assert_eq!(skill.license, None);
        assert!(skill.dependencies.is_empty());
    }

    #[test]
    fn accepts_crlf_and_an_empty_body() {
        let skill = SkillDocument::parse(&SKILL.replace('\n', "\r\n")).unwrap();
        assert_eq!(
            skill.body,
            "# Transfer\r\n\r\nBody with --- inside a line.\r\n"
        );
        let head = &SKILL[..SKILL.find("# Transfer").unwrap()];
        assert_eq!(SkillDocument::parse(head).unwrap().body, "");
        let unterminated = head.trim_end_matches('\n');
        assert_eq!(SkillDocument::parse(unterminated).unwrap().body, "");
    }

    #[test]
    fn requires_frontmatter() {
        for text in ["# Transfer\n", "", "---\nname: x\n", " ---\nname: x\n---\n"] {
            let err = SkillDocument::parse(text).unwrap_err();
            assert!(err.contains("must start with YAML frontmatter"), "{err}");
        }
    }

    #[test]
    fn errors_report_the_line_in_the_file() {
        let err = SkillDocument::parse(&SKILL.replace("revision: 2", "revision: two")).unwrap_err();
        assert!(err.ends_with("invalid u32 at line 8, column 11"), "{err}");
    }

    #[test]
    fn rejects_missing_unknown_and_invalid_fields() {
        for (edit, expected) in [
            (SKILL.replace("network: preprod\n", ""), "network"),
            (
                SKILL.replace("network: preprod", "network: devnet"),
                "devnet",
            ),
            (SKILL.replace("revision: 2", "revision: two"), "invalid u32"),
            (SKILL.replace("license:", "licence:"), "licence"),
            (
                SKILL.replace("    required_for: [transfer]\n", ""),
                "required_for",
            ),
        ] {
            let err = SkillDocument::parse(&edit).unwrap_err();
            assert!(err.starts_with("invalid frontmatter: "), "{err}");
            assert!(!err.contains("-->"), "snippet in {err}");
            assert!(err.contains(expected), "{expected}: {err}");
            assert!(!err.contains('\n'), "{err}");
        }
        for (edit, expected) in [
            (
                SKILL.replace("revision: 2", "revision: 0"),
                "frontmatter `revision` starts at 1",
            ),
            (
                SKILL.replace("name: transfer", "name: \"\""),
                "frontmatter `name` must not be empty",
            ),
        ] {
            assert_eq!(SkillDocument::parse(&edit).unwrap_err(), expected);
        }
    }
}
