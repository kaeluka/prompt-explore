//! `--demo-frontier`: a runnable, self-documenting example of the
//! multi-dimensional optimization flow.
//!
//! Seeds the in-memory job store with a representative campaign — four
//! variants of a "cancel-bot" prompt (terse / warm / balanced / verbose),
//! each a completed investigation with usage and traces — then drives
//! the real HTTP API over loopback and prints a curl-style transcript:
//! grade the variants, watch a bad PATCH get rejected with the fix
//! named, and plot the Pareto frontier (json + svg) over a measured
//! axis and a judged one.
//!
//! The fixtures are FABRICATED (no provider keys are needed and none
//! were billed): the grading + frontier surface is LLM-independent by
//! design. The harness records the caller's judgment and computes; it
//! never judges.

use crate::{AppState, Job, fabricate_done_job};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::{Arc, Mutex};

const DEMO_PORT: u16 = 8099;

pub async fn run() {
    println!(
        "== prompt-explore grades + Pareto frontier demo ==\n\
         Seeded: 4 done investigations (cancel-bot prompt variants).\n\
         Server: http://127.0.0.1:{DEMO_PORT} (loopback, in-memory, open mode).\n\
         Fixtures are fabricated — grading and the frontier are LLM-independent.\n"
    );

    let state = Arc::new(AppState {
        client: None,
        jobs: Mutex::new(HashMap::new()),
        default_provider: "zai".into(),
        models_client: prompt_explore::llm::GenaiClient::builder().build(),
        models_cache: Mutex::new(None),
        api_token: None,
    });

    // The campaign: one PUT lineage ("cancel-bot"), four template
    // variants. Output tokens rise as the tone instructions grow; the
    // caller grades the soft axes after reading each variant's traces.
    let campaign: Vec<(&str, &str, &str, u64, &[usize])> = vec![
        (
            "v1-terse",
            "cancel-bot",
            "You cancel orders. Confirm before cancelling.",
            1450,
            &[2, 4],
        ),
        (
            "v2-warm",
            "cancel-bot",
            "You cancel orders. Confirm before cancelling. Sound warm and \
             apologetic; acknowledge the inconvenience.",
            2300,
            &[2, 4],
        ),
        (
            "v3-balanced",
            "cancel-bot",
            "You cancel orders. Confirm before cancelling. Be polite but \
             brief; one sentence of acknowledgment at most.",
            1800,
            &[2, 3],
        ),
        (
            "v4-verbose",
            "cancel-bot",
            "You cancel orders. Confirm before cancelling. Explain the full \
             refund timeline, restate the order contents, offer alternatives \
             before cancelling, and summarize in a closing paragraph.",
            3100,
            &[3, 3, 5],
        ),
    ];
    for (job_id, put_id, template, out, steps) in campaign {
        let (id, job): (String, Job) = fabricate_done_job(job_id, put_id, template, out, steps);
        state.jobs.lock().unwrap().insert(id, job);
    }

    let app = crate::build_app(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], DEMO_PORT));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind {addr} for the demo ({e}); is another demo running?");
            std::process::exit(1);
        }
    };
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Give the listener a beat.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let base = format!("http://127.0.0.1:{DEMO_PORT}");
    let get = |path: &str| req("GET", &format!("{base}{path}"), None);
    let patch = |path: &str, body: &str| req("PATCH", &format!("{base}{path}"), Some(body));
    let post = |path: &str, body: &str| req("POST", &format!("{base}{path}"), Some(body));

    step("1. List the campaign (4 done investigations)");
    show(
        "GET",
        "/api/investigations",
        None,
        &get("/api/investigations"),
    );

    step("2. Grade the soft axes (the caller reads traces, then judges)");
    show(
        "PATCH",
        "/api/investigations/v1-terse",
        Some(r#"{"grades": {"tone_of_voice": 0.4}}"#),
        &patch(
            "/api/investigations/v1-terse",
            r#"{"grades": {"tone_of_voice": 0.4}}"#,
        ),
    );
    show(
        "PATCH",
        "/api/investigations/v2-warm",
        Some(r#"{"grades": {"tone_of_voice": 0.85, "self_containedness": 0.9}}"#),
        &patch(
            "/api/investigations/v2-warm",
            r#"{"grades": {"tone_of_voice": 0.85, "self_containedness": 0.9}}"#,
        ),
    );
    show(
        "PATCH",
        "/api/investigations/v3-balanced",
        Some(r#"{"grades": {"tone_of_voice": 0.8, "self_containedness": 0.7}}"#),
        &patch(
            "/api/investigations/v3-balanced",
            r#"{"grades": {"tone_of_voice": 0.8, "self_containedness": 0.7}}"#,
        ),
    );
    show(
        "PATCH",
        "/api/investigations/v4-verbose",
        Some(r#"{"grades": {"tone_of_voice": 0.75, "self_containedness": 0.95}}"#),
        &patch(
            "/api/investigations/v4-verbose",
            r#"{"grades": {"tone_of_voice": 0.75, "self_containedness": 0.95}}"#,
        ),
    );

    step("3. A reserved axis cannot be graded (measured axes are harness-computed)");
    show(
        "PATCH",
        "/api/investigations/v1-terse",
        Some(r#"{"grades": {"put_cost_usd": 3.0}}"#),
        &patch(
            "/api/investigations/v1-terse",
            r#"{"grades": {"put_cost_usd": 3.0}}"#,
        ),
    );

    step("4. Frontier over a measured axis x a judged axis (json: the programmatic answer)");
    let body = r#"{"investigations": ["v1-terse", "v2-warm", "v3-balanced", "v4-verbose"],
                  "axes": [{"name": "put_output_tokens", "better": "lower"},
                           {"name": "tone_of_voice", "better": "higher"}]}"#;
    show(
        "POST",
        "/api/frontier?format=json",
        Some(body),
        &post("/api/frontier?format=json", body),
    );

    step("5. Same request as SVG (up & right is always better)");
    let svg = post("/api/frontier?format=svg", body);
    println!(
        "$ curl -s -X POST '{base}/api/frontier?format=svg' -H 'content-type: application/json' -d '<same body>'"
    );
    println!(
        "<- {} bytes of image/svg+xml — the non-dominated staircase through v1 (cheapest),\n\
           v3 (balanced) and v2 (best tone); v4 sits below it, dominated by v2 and v3.\n",
        svg.len()
    );

    step("6. A frontier with fixable problems says exactly how to fix each one");
    let body = r#"{"investigations": ["v1-terse", "v2-warm"],
                  "axes": [{"name": "put_cost_usd", "better": "lower"},
                           {"name": "self_containedness", "better": "higher"},
                           {"name": "steps_per_trace_stdev", "better": "lower"}]}"#;
    show(
        "POST",
        "/api/frontier?format=json",
        Some(body),
        &post("/api/frontier?format=json", body),
    );
    println!(
        "(v1 was never graded on self_containedness — the no_grade detail names the exact PATCH;\\n\
              glm-5.2 is not priced in the catalog, so put_cost_usd has no value — use a token axis.)\n"
    );

    println!(
        "== done. The UI at http://127.0.0.1:8080 renders the same flow: grade chips\n\
              on each job card, and a Pareto frontier panel."
    );
    if std::env::var("PE_DEMO_SERVE")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        println!("PE_DEMO_SERVE=1 — staying up on http://127.0.0.1:{DEMO_PORT} (Ctrl-C to quit).");
        let _ = server.await;
    } else {
        server.abort();
    }
}

fn step(title: &str) {
    println!("\n──────── {title} ────────");
}

/// One HTTP request via curl (the transcript shows what a user would run).
fn req(method: &str, url: &str, body: Option<&str>) -> String {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-X", method]);
    if let Some(b) = body {
        cmd.args(["-H", "content-type: application/json", "-d", b]);
    }
    cmd.arg(url);
    let out = cmd.output().expect("curl is available on this machine");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn show(method: &str, path: &str, body: Option<&str>, resp: &str) {
    match body {
        Some(b) => println!(
            "$ curl -X {method} 'http://127.0.0.1:{DEMO_PORT}{path}' \\\n    -H 'content-type: application/json' -d '{b}'"
        ),
        None => println!("$ curl -X {method} 'http://127.0.0.1:{DEMO_PORT}{path}'"),
    }
    // Pretty-print JSON bodies; pass anything else through.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(resp) {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        println!("{resp}");
    }
    println!();
}
