//! Public-service limits over HTTP, in process, against a scripted TRP: the
//! daily quota, the concurrency gate, the global cutoff, and `GET /metrics`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use fluent_server::limits::{Gate, Limits, Quota};
use fluent_server::store::Store;
use serde_json::{Value, json};

const TOKEN: &str = "limits-test-token";
const API_KEY: &str = "limits-test-trp-key";

fn env() -> [(&'static str, &'static str); 2] {
    [
        ("FLUENT_API_TOKEN", TOKEN),
        ("TRP_PREPROD_API_KEY", API_KEY),
    ]
}

/// Starts a server against `trp_url` under `limits`, and opens a session.
async fn start(trp_url: &str, limits: Limits) -> (Running, String) {
    let config = load_config(&limited_config_text(trp_url, ""), &env());
    let server = start_limited(&config, limits).await;
    let (status, session) = server.initialize(Some(TOKEN)).await;
    assert_eq!(status, 200);
    (server, session.expect("a session id"))
}

fn gate(max: usize, wait: Duration, cutoff: Duration) -> Gate {
    Gate::new(max, wait, cutoff)
}

fn code(result: &Value) -> &str {
    result["error"]["code"].as_str().unwrap_or("ok")
}

#[tokio::test]
async fn the_call_past_the_daily_quota_is_refused_with_its_reset_time() {
    let trp = resolver(Duration::ZERO).await;
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(&dir.path().join("fluent.sqlite"))
        .await
        .expect("open store");
    let limits = Limits {
        gate: Gate::default(),
        quota: Some(Quota::new(store, 2)),
    };
    let (server, session) = start(&trp.uri(), limits).await;

    for _ in 0..2 {
        let result = call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()).await;
        assert_eq!(result["status"], "prepared_unsigned", "{result}");
    }
    let refused = call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()).await;
    assert_eq!(code(&refused), "quota_exhausted", "{refused}");
    let details = &refused["error"]["details"];
    assert_eq!(details["limit"], 2);
    let resets_at = details["resets_at"].as_str().expect("resets_at");
    assert!(resets_at.ends_with("T00:00:00Z"), "{resets_at}");
    // The refused call never reached the resolver.
    let resolved = trp.received_requests().await.unwrap_or_default();
    assert_eq!(resolved.len(), 2);

    // Skill and address tools are not metered.
    let address = call(
        &server,
        TOKEN,
        &session,
        "fluent_inspect_address",
        json!({ "address": RECEIVER }),
    )
    .await;
    assert_eq!(code(&address), "ok", "{address}");
}

#[tokio::test]
async fn a_full_server_turns_calls_away_as_busy() {
    let trp = resolver(Duration::from_millis(1500)).await;
    let limits = Limits {
        gate: gate(1, Duration::from_millis(200), Duration::from_secs(20)),
        quota: None,
    };
    let (server, session) = start(&trp.uri(), limits).await;
    // A second session, because the test's calls share one JSON-RPC id.
    let (_, other) = server.initialize(Some(TOKEN)).await;
    let other = other.expect("a session id");

    let (first, second) = tokio::join!(
        call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            call(&server, TOKEN, &other, TRANSFER_TOOL, transfer_args()).await
        },
    );
    assert_eq!(first["status"], "prepared_unsigned", "{first}");
    assert_eq!(code(&second), "resolver_unavailable", "{second}");
    assert_eq!(second["error"]["details"]["reason"], "server_busy");

    // Once the slot frees up, calls are admitted again.
    let third = call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()).await;
    assert_eq!(third["status"], "prepared_unsigned", "{third}");
}

#[tokio::test]
async fn the_global_cutoff_fails_a_slow_call_as_a_resolver_timeout() {
    // The engine's own resolver timeout is 1 second; the cutoff comes first.
    let trp = resolver(Duration::from_secs(5)).await;
    let limits = Limits {
        gate: gate(4, Duration::from_secs(5), Duration::from_millis(300)),
        quota: None,
    };
    let config = load_config(
        &limited_config_text(
            &trp.uri(),
            "resolver_timeout_secs = 1\nglobal_cutoff_secs = 1",
        ),
        &env(),
    );
    let server = start_limited(&config, limits).await;
    let (_, session) = server.initialize(Some(TOKEN)).await;
    let session = session.expect("a session id");

    let started = Instant::now();
    let result = call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()).await;
    assert_eq!(code(&result), "resolver_timeout", "{result}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn metrics_show_every_series_after_one_call() {
    let trp = resolver(Duration::ZERO).await;
    let (server, session) = start(&trp.uri(), Limits::default()).await;
    let result = call(&server, TOKEN, &session, TRANSFER_TOOL, transfer_args()).await;
    assert_eq!(result["status"], "prepared_unsigned", "{result}");

    let response = server
        .client
        .get(server.url("/metrics"))
        .send()
        .await
        .expect("GET /metrics");
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("metrics body");
    for series in [
        "fluent_prepare_total{registration=\"transfer_preprod\",tx=\"transfer\",outcome=\"ok\"}",
        "fluent_prepare_duration_seconds_bucket{registration=\"transfer_preprod\",tx=\"transfer\",outcome=\"ok\",le=",
        "fluent_prepare_duration_seconds_count{",
        "fluent_quota_rejections_total",
        "fluent_inflight_resolutions",
        "fluent_sessions_active",
        "process_resident_memory_bytes",
        "process_cpu_seconds_total",
    ] {
        assert!(body.contains(series), "no {series} in:\n{body}");
    }
    for secret in [TOKEN, API_KEY, SENDER, RECEIVER] {
        assert!(!body.contains(secret), "the metrics contain {secret}");
    }
}

#[tokio::test]
async fn proxied_metrics_requests_need_a_token() {
    let trp = resolver(Duration::ZERO).await;
    let (server, _) = start(&trp.uri(), Limits::default()).await;
    let get = |token: Option<&str>| {
        let mut request = server
            .client
            .get(server.url("/metrics"))
            .header("x-forwarded-for", "203.0.113.7");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request.send()
    };
    assert_eq!(get(None).await.expect("GET /metrics").status(), 401);
    assert_eq!(
        get(Some("wrong")).await.expect("GET /metrics").status(),
        401
    );
    assert_eq!(get(Some(TOKEN)).await.expect("GET /metrics").status(), 200);
}

#[tokio::test]
async fn a_metrics_token_opens_metrics_to_proxied_callers() {
    let trp = resolver(Duration::ZERO).await;
    let text = limited_config_text(&trp.uri(), "").replace(
        "[server]\n",
        "[server]\nmetrics_token_env = \"FLUENT_METRICS_TOKEN\"\n",
    );
    let mut with_token = env().to_vec();
    with_token.push(("FLUENT_METRICS_TOKEN", "scrape-token"));
    let config = load_config(&text, &with_token);
    let server = start_limited(&config, Limits::default()).await;
    let response = server
        .client
        .get(server.url("/metrics"))
        .header("forwarded", "for=203.0.113.7")
        .header("authorization", "Bearer scrape-token")
        .send()
        .await
        .expect("GET /metrics");
    assert_eq!(response.status(), 200);

    // An unset metrics token refuses to start rather than leave it open.
    let unset = load_config(&text, &env());
    let err = bind(&unset).await.err().expect("bind should fail");
    assert!(err.to_string().contains("FLUENT_METRICS_TOKEN"), "{err}");
}
