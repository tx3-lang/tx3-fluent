//! A Markdown transcript of the tools a server listed and called, rendered
//! offline from its JSON log lines.
//!
//! `fluent serve` logs, at `info`, one line per `tools/list` answered (the
//! principal's `sub_hash` and the listed tool names) and one per tool call
//! (inside the `tool_call` span: the tool, `sub_hash`, `registration`, `tx`,
//! the argument *names*, `outcome` and `duration_ms`). [`read`] picks those
//! lines out of any log text, such as saved `kubectl logs` output, and
//! ignores everything else. Nothing is recorded that the logs do not hold:
//! no argument value, no result body, no transaction hash.
//!
//! A transcript pairs with a client's conversation export by time, principal
//! and tool.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

/// The target of the server's MCP handler logs.
const TARGET: &str = "fluent_server::mcp";

/// One `tools/list` or `tools/call` the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The log line's timestamp, as logged (RFC 3339, UTC).
    pub time: String,
    /// The principal's `sub_hash`; `None` over stdio or without
    /// authentication.
    pub sub_hash: Option<String>,
    /// What was answered.
    pub kind: Kind,
}

/// What an [`Entry`] answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A `tools/list`, with the tool names listed.
    List {
        /// The listed tool names, in the order listed.
        tools: Vec<String>,
    },
    /// A `tools/call`.
    Call {
        /// The tool called.
        tool: String,
        /// The registration and transaction behind a transaction tool.
        transaction: Option<(String, String)>,
        /// The argument names sent, never their values.
        arguments: Vec<String>,
        /// `ok`, an error code, or `unknown_tool`.
        outcome: String,
        /// How long the call took, when logged.
        duration_ms: Option<u64>,
    },
}

/// The entries read from log text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    /// The entries, ordered by time.
    pub entries: Vec<Entry>,
    /// How many lines were read, entries or not.
    pub lines: usize,
}

/// Reads the entries out of `logs`: JSON log lines, each optionally prefixed
/// (as `kubectl logs --timestamps` or `--prefix` do); other lines are
/// skipped. Entries are ordered by time, so logs from several files or pods
/// can be read together.
pub fn read(logs: &str) -> Transcript {
    let mut transcript = Transcript::default();
    for line in logs.lines() {
        transcript.lines += 1;
        if let Some(entry) = line.find('{').and_then(|start| entry(&line[start..])) {
            transcript.entries.push(entry);
        }
    }
    transcript.entries.sort_by(|a, b| a.time.cmp(&b.time));
    transcript
}

/// The entry a log line records, if any.
fn entry(json: &str) -> Option<Entry> {
    let line: Value = serde_json::from_str(json).ok()?;
    if line.get("target")?.as_str()? != TARGET {
        return None;
    }
    let time = line.get("timestamp")?.as_str()?.to_string();
    let fields = line.get("fields")?;
    let text = |value: &Value, key: &str| value.get(key)?.as_str().map(str::to_string);
    let names = |list: &str| {
        list.split(',')
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    if let Some(tools) = text(fields, "tools") {
        return Some(Entry {
            time,
            sub_hash: text(fields, "sub_hash"),
            kind: Kind::List {
                tools: names(&tools),
            },
        });
    }

    let outcome = text(fields, "outcome")?;
    let span = line
        .get("spans")?
        .as_array()?
        .iter()
        .rev()
        .find(|span| span.get("name").and_then(Value::as_str) == Some("tool_call"))?;
    let transaction = text(span, "registration").zip(text(span, "tx"));
    Some(Entry {
        time,
        sub_hash: text(span, "sub_hash"),
        kind: Kind::Call {
            tool: text(fields, "tool").or_else(|| text(span, "tool"))?,
            transaction,
            arguments: names(&text(span, "arguments").unwrap_or_default()),
            outcome,
            duration_ms: fields
                .get("duration_ms")
                .or_else(|| span.get("duration_ms"))
                .and_then(Value::as_u64),
        },
    })
}

impl Transcript {
    /// Keeps only the entries of the principals whose `sub_hash` is listed.
    pub fn only(&mut self, sub_hashes: &[String]) {
        self.entries.retain(|entry| {
            entry
                .sub_hash
                .as_ref()
                .is_some_and(|hash| sub_hashes.contains(hash))
        });
    }

    /// The transcript as Markdown, naming `source` (the logs it was read
    /// from) in its header.
    pub fn to_markdown(&self, source: &str) -> String {
        let lists = self
            .entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::List { .. }))
            .count();
        let mut principals: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &self.entries {
            *principals.entry(principal(entry)).or_default() += 1;
        }
        let principals = principals
            .iter()
            .map(|(principal, count)| format!("{principal} ({count})"))
            .collect::<Vec<_>>();

        let mut text = String::from("# Tx3 Fluent transcript\n\n");
        let _ = writeln!(
            text,
            "- Rendered by: `cargo xtask transcript` from {source}"
        );
        let _ = writeln!(text, "- Log lines read: {}", self.lines);
        let _ = writeln!(
            text,
            "- Entries: {} ({lists} `tools/list`, {} `tools/call`)",
            self.entries.len(),
            self.entries.len() - lists
        );
        if let (Some(first), Some(last)) = (self.entries.first(), self.entries.last()) {
            let _ = writeln!(text, "- From {} to {}", first.time, last.time);
        }
        if !principals.is_empty() {
            let _ = writeln!(text, "- Principals: {}", principals.join(", "));
        }
        text.push_str(
            "\nEach section is one `tools/list` or `tools/call` the server answered, read \
             from its logs. The logs hold tool and argument names, outcomes and the \
             principal's hashed subject; never argument values, results or transaction \
             hashes. Pair each section with the client's conversation export by time, \
             principal and tool.\n",
        );
        for (index, entry) in self.entries.iter().enumerate() {
            text.push_str(&section(index + 1, entry));
        }
        text
    }
}

/// `sub:<hash>`, or how an unauthenticated session is shown.
fn principal(entry: &Entry) -> String {
    match &entry.sub_hash {
        Some(hash) => format!("`sub:{hash}`"),
        None => "local (stdio or unauthenticated)".to_string(),
    }
}

/// One transcript section.
fn section(number: usize, entry: &Entry) -> String {
    let code_list = |names: &[String]| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut text = String::new();
    match &entry.kind {
        Kind::List { tools } => {
            let _ = writeln!(text, "\n## {number}. `tools/list`\n");
            let _ = writeln!(text, "- Time: {}", entry.time);
            let _ = writeln!(text, "- Principal: {}", principal(entry));
            let _ = writeln!(text, "- Tools ({}): {}", tools.len(), code_list(tools));
        }
        Kind::Call {
            tool,
            transaction,
            arguments,
            outcome,
            duration_ms,
        } => {
            let _ = writeln!(text, "\n## {number}. `tools/call` · `{tool}`\n");
            let _ = writeln!(text, "- Time: {}", entry.time);
            let _ = writeln!(text, "- Principal: {}", principal(entry));
            if let Some((registration, tx)) = transaction {
                let _ = writeln!(text, "- Transaction: `{tx}` of `{registration}`");
            }
            let _ = writeln!(text, "- Arguments: {}", code_list(arguments));
            match duration_ms {
                Some(ms) => {
                    let _ = writeln!(text, "- Outcome: `{outcome}` in {ms} ms");
                }
                None => {
                    let _ = writeln!(text, "- Outcome: `{outcome}`");
                }
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"timestamp":"2026-09-29T23:16:47.866382Z","level":"INFO","fields":{"message":"listed the session's tools","sub_hash":"0123456789ab","tools":"fluent_get_skill,fluent_inspect_address","count":2},"target":"fluent_server::mcp"}"#;
    const CALL: &str = r#"{"timestamp":"2026-09-29T23:16:48.000000Z","level":"INFO","fields":{"message":"tool call failed","tool":"transfer_preprod_transfer","code":"invalid_arguments","outcome":"invalid_arguments","duration_ms":3},"target":"fluent_server::mcp","span":{"name":"tool_call"},"spans":[{"name":"serve_inner"},{"arguments":"quantity,sender","sub_hash":"0123456789ab","registration":"transfer_preprod","tx":"transfer","outcome":"invalid_arguments","duration_ms":3,"tool":"transfer_preprod_transfer","name":"tool_call"}]}"#;

    #[test]
    fn reads_lists_and_calls_and_skips_everything_else() {
        let logs = format!(
            "not json\n2026-09-29T23:16:48Z {CALL}\n{{\"target\":\"rmcp::service\"}}\n{LIST}\n"
        );
        let transcript = read(&logs);
        assert_eq!(transcript.lines, 4);
        assert_eq!(
            transcript.entries,
            [
                Entry {
                    time: "2026-09-29T23:16:47.866382Z".into(),
                    sub_hash: Some("0123456789ab".into()),
                    kind: Kind::List {
                        tools: vec!["fluent_get_skill".into(), "fluent_inspect_address".into()],
                    },
                },
                Entry {
                    time: "2026-09-29T23:16:48.000000Z".into(),
                    sub_hash: Some("0123456789ab".into()),
                    kind: Kind::Call {
                        tool: "transfer_preprod_transfer".into(),
                        transaction: Some(("transfer_preprod".into(), "transfer".into())),
                        arguments: vec!["quantity".into(), "sender".into()],
                        outcome: "invalid_arguments".into(),
                        duration_ms: Some(3),
                    },
                },
            ]
        );
    }

    #[test]
    fn sections_list_names_and_outcomes() {
        let text = read(&format!("{LIST}\n{CALL}\n")).to_markdown("`test.log`");
        assert!(text.starts_with("# Tx3 Fluent transcript\n\n- Rendered by: `cargo xtask transcript` from `test.log`\n- Log lines read: 2\n- Entries: 2 (1 `tools/list`, 1 `tools/call`)\n"), "{text}");
        assert!(
            text.contains("- Principals: `sub:0123456789ab` (2)\n"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "\n## 1. `tools/list`\n\n\
             - Time: 2026-09-29T23:16:47.866382Z\n\
             - Principal: `sub:0123456789ab`\n\
             - Tools (2): `fluent_get_skill`, `fluent_inspect_address`\n\
             \n## 2. `tools/call` · `transfer_preprod_transfer`\n\n\
             - Time: 2026-09-29T23:16:48.000000Z\n\
             - Principal: `sub:0123456789ab`\n\
             - Transaction: `transfer` of `transfer_preprod`\n\
             - Arguments: `quantity`, `sender`\n\
             - Outcome: `invalid_arguments` in 3 ms\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn only_keeps_the_named_principals() {
        let mut transcript = read(&format!("{LIST}\n{CALL}\n"));
        transcript.only(&["ffffffffffff".to_string()]);
        assert!(transcript.entries.is_empty());
        let mut transcript = read(&format!("{LIST}\n{CALL}\n"));
        transcript.only(&["0123456789ab".to_string()]);
        assert_eq!(transcript.entries.len(), 2);
    }
}
