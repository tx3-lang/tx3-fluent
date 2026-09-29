//! Hosted and self-hosted parity: for each hosted bundle, the `fluent`
//! binary serving it self-hosted (over stdio, and over HTTP in `token` mode)
//! and hosted (over HTTP in `oidc` mode, for a user who selected it) answers
//! `tools/list` and one `tools/call` with byte-identical JSON-RPC messages.
//!
//! Each mode is sent the same messages with the same JSON-RPC ids, so the
//! only transport-specific parts are the framing (a stdout line, or an HTTP
//! body or SSE event) and the HTTP session headers, which are left out; the
//! message text itself must be equal byte for byte.
//!
//! The call prepares a transaction against a `wiremock` TRP resolver that
//! replays a recorded `trp.resolve` response from
//! `fluent-core/tests/fixtures/trp/{slug}.json`:
//!
//! | Bundle | Recorded transaction |
//! | --- | --- |
//! | `transfer_preprod` | `b2698db1…8f73`, the preprod transfer in `tx/transfer-preprod.hex` |
//! | `strike_staking_mainnet` | `22d2ae9c…4875`, a mainnet Strike `withdraw_stake`, CBOR from Koios `tx_cbor` (also `tx/strike-withdraw-mainnet.hex`) |
//!
//! Both bundles are run as the loader fixtures in
//! `registrations/valid/`, and `transfer_preprod` also as the reviewed hosted
//! bundle in `deploy/hosted/registrations/` (`open-tx3/transfer`), whose
//! pinned registry artifact is served offline from `fixtures/registry/`.
//! Strike has no hosted bundle yet: `open-tx3/strike-staking:0.2.0` is not
//! published.
//!
//! The hosted run also checks the selection is what scopes it: a second user,
//! who selected nothing, sees only the fixed tools and cannot call the
//! bundle's.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use common::{PUBLIC_URL, claims, fixtures, initialize, jwks, keys, list_tools, oidc_auth};
use fluent_core::registration::load_dir;
use fluent_server::store::{Select, Store};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// How long one response or startup may take before the test fails instead
/// of hanging.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The static bearer token of the `token` mode.
const API_TOKEN: &str = "parity-test-token";

/// The hosted user who selected the bundle, and one who selected nothing.
const SELECTED_USER: &str = "auth0|parity-selected";
const OTHER_USER: &str = "auth0|parity-other";

/// Where a case's bundle comes from.
#[derive(Clone, Copy)]
enum Bundle {
    /// `fluent-core/tests/fixtures/registrations/valid/{slug}`.
    Fixture,
    /// `deploy/hosted/registrations/{slug}`, fetched from a local registry.
    Hosted,
}

/// One hosted bundle and the call the parity run makes.
#[derive(Clone)]
struct Case {
    bundle: Bundle,
    slug: &'static str,
    /// The protocol scope the bundle declares.
    scope: &'static str,
    network: &'static str,
    tool: &'static str,
    arguments: Value,
    tx_hash: &'static str,
}

fn transfer() -> Case {
    Case {
        bundle: Bundle::Fixture,
        slug: "transfer_preprod",
        scope: "unknown",
        network: "preprod",
        tool: "transfer_preprod_transfer",
        arguments: common::transfer_args(),
        tx_hash: common::TRANSFER_HASH,
    }
}

/// The owner, staking UTxO and staker address of the recorded withdrawal:
/// its input, its required signer, and its one output's address.
fn strike() -> Case {
    Case {
        bundle: Bundle::Fixture,
        scope: "strike-finance",
        slug: "strike_staking_mainnet",
        network: "mainnet",
        tool: "strike_staking_mainnet_withdraw_stake",
        arguments: json!({
            "owner_pkh": "fd9de51c9c700d8c5c0f9adccf1daee47a6119922feae24f10771c56",
            "staking_utxo": "df2e035dfb80b27aafaa766818be5375f836a0c0d05c6b045928e375e0e122fe#0",
            "staker": "addr1q87emegun3cqmrzup7ddenca4mj85cgejgh74cj0zpm3c435zrq0euvqekgale2ak7rza3lafk4gq93lxm2w2gk2mmcs0uj0c2"
        }),
        tx_hash: "22d2ae9c4d076acac2779bfcc1a2a327b788bfafe9022a7c5e81c8671f824875",
    }
}

/// The reviewed hosted transfer, `open-tx3/transfer`: no middleman.
fn hosted_transfer() -> Case {
    Case {
        bundle: Bundle::Hosted,
        scope: "open-tx3",
        arguments: json!({
            "quantity": 3_000_000,
            "sender": common::SENDER,
            "receiver": common::RECEIVER
        }),
        ..transfer()
    }
}

/// The `tools/call` message every mode is sent.
fn call_message(case: &Case) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": case.tool, "arguments": case.arguments}
    })
}

/// A resolver answering `trp.resolve` with the case's recorded response.
async fn resolver(case: &Case) -> MockServer {
    let recorded = std::fs::read(fixtures().join(format!("trp/{}.json", case.slug)))
        .expect("the recorded TRP response");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "trp.resolve" })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(recorded, "application/json"))
        .mount(&server)
        .await;
    server
}

/// A directory holding only the case's bundle, a store and the
/// configurations.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    /// Copies the case's bundle; a hosted bundle's registry URL becomes
    /// `registry`.
    fn new(case: &Case, registry: &str) -> Scratch {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("registrations").join(case.slug);
        match case.bundle {
            Bundle::Fixture => copy_dir(
                &fixtures().join("registrations/valid").join(case.slug),
                &target,
            ),
            Bundle::Hosted => {
                copy_dir(&hosted_dir().join(case.slug), &target);
                let manifest = target.join("registration.toml");
                let text = std::fs::read_to_string(&manifest).expect("read manifest");
                let line = format!("url = \"{HOSTED_REGISTRY}\"");
                assert_eq!(text.matches(&line).count(), 1, "{line}");
                let text = text.replace(&line, &format!("url = \"{registry}\""));
                std::fs::write(&manifest, text).expect("write manifest");
            }
        }
        Scratch { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Writes configuration `name`: the bundle, the case's network resolved
    /// by `trp_url`, and `rest` (`[server]`, `[auth]` and more).
    fn config(&self, name: &str, case: &Case, trp_url: &str, rest: &str) -> PathBuf {
        let text = format!(
            "[registrations]\ndir = {}\n\n[networks.{}]\ntrp_url = \"{trp_url}\"\n\n{rest}",
            toml::Value::String(self.path("registrations").display().to_string()),
            case.network,
        );
        let path = self.path(name);
        std::fs::write(&path, text).expect("write configuration");
        path
    }
}

/// The registry every hosted bundle is pinned to.
const HOSTED_REGISTRY: &str = "https://oci.tx3.land";

fn hosted_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/hosted/registrations")
}

/// A local registry serving every pinned artifact in `fixtures/registry/`
/// (`{repository}/{tag}/manifest.json` and `protocol.tii`).
async fn registry() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let root = fixtures().join("registry");
    for scope in std::fs::read_dir(&root).expect("read the registry fixtures") {
        let scope = scope.expect("entry").path();
        for name in std::fs::read_dir(&scope).expect("read scope") {
            let name = name.expect("entry").path();
            for tag in std::fs::read_dir(&name).expect("read repository") {
                let tag = tag.expect("entry").path();
                let repository = format!(
                    "{}/{}",
                    scope.file_name().expect("a file name").to_string_lossy(),
                    name.file_name().expect("a file name").to_string_lossy()
                );
                let tag_name = tag
                    .file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned();
                let manifest = std::fs::read(tag.join("manifest.json")).expect("manifest");
                let tii = std::fs::read(tag.join("protocol.tii")).expect("TII");
                Mock::given(method("GET"))
                    .and(path(format!("/v2/{repository}/manifests/{tag_name}")))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_raw(manifest, "application/vnd.oci.image.manifest.v1+json"),
                    )
                    .mount(&server)
                    .await;
                Mock::given(method("GET"))
                    .and(path(format!(
                        "/v2/{repository}/blobs/{}",
                        fluent_core::registration::sha256_digest(&tii)
                    )))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_raw(tii, "application/octet-stream"),
                    )
                    .mount(&server)
                    .await;
            }
        }
    }
    server
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create directory");
    for entry in std::fs::read_dir(from).expect("read directory") {
        let entry = entry.expect("directory entry");
        std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy file");
    }
}

/// A running `fluent` process, killed on drop; its stdout and stderr lines
/// arrive on channels.
struct Fluent {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
}

impl Fluent {
    fn spawn(args: &[&str], config: &Path, env: &[(&str, &str)]) -> Fluent {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fluent"))
            .args(args)
            .arg("--config")
            .arg(config)
            .env_clear()
            .envs(env.iter().copied())
            .env("RUST_LOG", "info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fluent");
        let stdin = child.stdin.take();
        let stdout = lines(child.stdout.take().expect("stdout"));
        let stderr = lines(child.stderr.take().expect("stderr"));
        Fluent {
            child,
            stdin,
            stdout,
            stderr,
        }
    }

    /// `serve --http`, once it logs the address it listens on.
    fn http(config: &Path, env: &[(&str, &str)]) -> (Fluent, String) {
        let fluent = Fluent::spawn(&["serve", "--http"], config, env);
        loop {
            let line = fluent
                .stderr
                .recv_timeout(TIMEOUT)
                .expect("fluent serve --http to start");
            let Ok(log) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if log["fields"]["message"] == "serving MCP over HTTP" {
                let address = log["fields"]["address"].as_str().expect("an address");
                return (fluent, format!("http://{address}/mcp"));
            }
        }
    }

    /// Writes one JSON-RPC line and returns the text of the reply with
    /// `id`, skipping notifications.
    fn request(&mut self, message: &Value) -> String {
        let stdin = self.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{message}").expect("write to fluent");
        stdin.flush().expect("flush to fluent");
        loop {
            let line = self
                .stdout
                .recv_timeout(TIMEOUT)
                .unwrap_or_else(|err| panic!("no reply to {message}: {err}"));
            let reply: Value =
                serde_json::from_str(&line).unwrap_or_else(|err| panic!("{err}: {line}"));
            if reply.get("id") == message.get("id") {
                return line;
            }
        }
    }

    fn notify(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{message}").expect("write to fluent");
        stdin.flush().expect("flush to fluent");
    }
}

impl Drop for Fluent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lines(stream: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// The raw text of `tools/list` and `tools/call` replies.
#[derive(Debug)]
struct Exchange {
    list: String,
    call: String,
}

/// Over stdio: initialize, list, call.
fn over_stdio(config: &Path, case: &Case) -> Exchange {
    let mut fluent = Fluent::spawn(&["serve", "--stdio"], config, &[]);
    let init = fluent.request(&initialize());
    assert!(init.contains("\"result\""), "{init}");
    fluent.notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    Exchange {
        list: fluent.request(&list_tools()),
        call: fluent.request(&call_message(case)),
    }
}

/// An MCP session over HTTP at `url`, as `token`.
struct HttpSession {
    client: reqwest::Client,
    url: String,
    token: String,
    session: String,
}

impl HttpSession {
    async fn open(url: &str, token: &str) -> HttpSession {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("HTTP client");
        let mut session = HttpSession {
            client,
            url: url.to_string(),
            token: token.to_string(),
            session: String::new(),
        };
        let response = session.post(&initialize()).await;
        assert_eq!(response.status(), 200, "initialize");
        session.session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .expect("a session id")
            .to_string();
        let init = reply_text(response).await;
        assert!(init.contains("\"result\""), "{init}");
        let ack = session
            .post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        assert!(ack.status().is_success(), "{}", ack.status());
        session
    }

    async fn post(&self, message: &Value) -> reqwest::Response {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("authorization", format!("Bearer {}", self.token))
            .body(message.to_string());
        if !self.session.is_empty() {
            request = request
                .header("mcp-session-id", &self.session)
                .header("mcp-protocol-version", common::PROTOCOL_VERSION);
        }
        request.send().await.expect("POST /mcp")
    }

    /// The raw text of the reply to `message`.
    async fn request(&self, message: &Value) -> String {
        let response = self.post(message).await;
        assert_eq!(response.status(), 200, "{message}");
        reply_text(response).await
    }

    async fn exchange(&self, case: &Case) -> Exchange {
        Exchange {
            list: self.request(&list_tools()).await,
            call: self.request(&call_message(case)).await,
        }
    }
}

/// The JSON-RPC reply in a response body, sent as JSON or as an SSE event,
/// exactly as the server wrote it.
async fn reply_text(response: reqwest::Response) -> String {
    let body = response.text().await.expect("response body");
    if serde_json::from_str::<Value>(&body).is_ok() {
        return body;
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .find(|data| {
            serde_json::from_str::<Value>(data).is_ok_and(|message| message.get("id").is_some())
        })
        .unwrap_or_else(|| panic!("no JSON-RPC reply in {body:?}"))
        .to_string()
}

/// Over HTTP in `token` mode.
async fn over_token(scratch: &Scratch, case: &Case, trp_url: &str) -> Exchange {
    let config = scratch.config(
        "token.toml",
        case,
        trp_url,
        "[server]\nlisten = \"127.0.0.1:0\"\n\n\
         [auth]\nmode = \"token\"\ntoken_env = \"FLUENT_API_TOKEN\"\n",
    );
    let (fluent, url) =
        blocking(move || Fluent::http(&config, &[("FLUENT_API_TOKEN", API_TOKEN)])).await;
    let exchange = HttpSession::open(&url, API_TOKEN)
        .await
        .exchange(case)
        .await;
    drop(fluent);
    exchange
}

/// Over HTTP in `oidc` mode with a `[store]` where [`SELECTED_USER`] selected
/// the bundle; also checks [`OTHER_USER`] sees none of its tools.
async fn over_oidc(scratch: &Scratch, case: &Case, trp_url: &str) -> Exchange {
    let issuer = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[&keys().0])))
        .mount(&issuer)
        .await;

    let store_path = scratch.path("fluent.sqlite");
    let registrations = load_dir(&scratch.path("registrations"))
        .await
        .expect("load the bundle");
    let revision = registrations
        .catalog
        .get(case.slug)
        .expect("the bundle's registration")
        .revision()
        .to_string();
    let store = Store::open(&store_path).await.expect("open the store");
    store
        .set_selection(
            SELECTED_USER,
            case.slug,
            Select::On {
                revision: &revision,
            },
        )
        .await
        .expect("select the bundle");
    drop(store);

    let config = scratch.config(
        "oidc.toml",
        case,
        trp_url,
        &format!(
            "[server]\nlisten = \"127.0.0.1:0\"\npublic_url = \"{PUBLIC_URL}\"\n\n\
             [store]\nsqlite_path = {}\n\n\
             [auth]\n{}\n",
            toml::Value::String(store_path.display().to_string()),
            oidc_auth(&format!("{}/jwks.json", issuer.uri())),
        ),
    );
    let (fluent, url) = blocking(move || Fluent::http(&config, &[])).await;

    let token = keys().0.sign(&claims(SELECTED_USER));
    let exchange = HttpSession::open(&url, &token).await.exchange(case).await;

    let other = HttpSession::open(&url, &keys().0.sign(&claims(OTHER_USER))).await;
    let listed: Value = serde_json::from_str(&other.request(&list_tools()).await).expect("JSON");
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["fluent_get_skill", "fluent_inspect_address"],
        "a user who selected nothing"
    );
    let refused: Value =
        serde_json::from_str(&other.request(&call_message(case)).await).expect("JSON");
    let refused = common::tool_result(&refused);
    assert_eq!(
        refused["error"]["code"], "registration_unavailable",
        "{refused}"
    );

    drop(fluent);
    exchange
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("blocking task")
}

/// Runs the three modes over `case` and compares them.
async fn check_parity(case: Case) {
    let trp = resolver(&case).await;
    let trp_url = trp.uri();
    let registry = registry().await;
    let scratch = Scratch::new(&case, &registry.uri());

    let stdio_config = scratch.config("stdio.toml", &case, &trp_url, "[auth]\nmode = \"none\"\n");
    let stdio = {
        let case = case.clone();
        blocking(move || over_stdio(&stdio_config, &case)).await
    };
    let token = over_token(&scratch, &case, &trp_url).await;
    let oidc = over_oidc(&scratch, &case, &trp_url).await;

    // The self-hosted stdio reply is the reference: a listing with the fixed
    // tools and the bundle's, and a prepared, unsigned transaction.
    let list: Value = serde_json::from_str(&stdio.list).expect("tools/list JSON");
    let tools = list["result"]["tools"].as_array().expect("tools");
    assert!(tools.len() > 2, "{list}");
    for tool in &tools[2..] {
        let name = tool["name"].as_str().expect("a tool name");
        assert!(name.starts_with(case.slug), "{name}");
    }
    let call: Value = serde_json::from_str(&stdio.call).expect("tools/call JSON");
    assert_eq!(call["result"]["isError"], false, "{call}");
    let envelope = &call["result"]["structuredContent"];
    assert_eq!(envelope["status"], "prepared_unsigned");
    assert_eq!(envelope["tx_hash"], case.tx_hash);
    assert_eq!(envelope["network"], case.network);
    assert_eq!(envelope["protocol"]["registration_slug"], case.slug);
    assert_eq!(envelope["protocol"]["scope"], case.scope);

    for (mode, exchange) in [("HTTP token", &token), ("HTTP oidc", &oidc)] {
        assert_eq!(
            exchange.list, stdio.list,
            "{}: tools/list differs from stdio in {mode} mode",
            case.slug
        );
        assert_eq!(
            exchange.call, stdio.call,
            "{}: tools/call differs from stdio in {mode} mode",
            case.slug
        );
    }
    let resolved = trp.received_requests().await.expect("recorded requests");
    assert_eq!(resolved.len(), 3, "one resolution per mode");
}

#[tokio::test(flavor = "multi_thread")]
async fn transfer_preprod_is_the_same_hosted_and_self_hosted() {
    check_parity(transfer()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hosted_transfer_preprod_is_the_same_hosted_and_self_hosted() {
    check_parity(hosted_transfer()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn strike_staking_mainnet_is_the_same_hosted_and_self_hosted() {
    check_parity(strike()).await;
}
