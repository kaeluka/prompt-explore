use super::*;
use crate::llm::track::{UsageByRole, UsageTotals};
use std::collections::BTreeMap;

fn snap(id: &str) -> InvestigationSnapshot {
    InvestigationSnapshot {
        id: id.into(),
        status: SnapshotStatus::Done,
        put_id: Some(format!("put-{id}")),
        grades: BTreeMap::new(),
        usage: Some(UsageByRole::default()),
        put_model: Some("zai_coding::glm-5.2".into()),
        sim_model: Some("zai_coding::glm-5.2".into()),
        steps_per_trace: vec![2, 4],
    }
}

fn usage(put_out: u64) -> UsageByRole {
    UsageByRole {
        put: UsageTotals {
            input_tokens: 1000,
            output_tokens: put_out,
            ..Default::default()
        },
        sim: UsageTotals::default(),
    }
}

fn parse_req(json: &str) -> FrontierRequest {
    serde_json::from_str(json).unwrap()
}

// ---------------------------------------------------------------------
// Allow-patterns
// ---------------------------------------------------------------------

#[test]
fn grade_axis_name_pattern() {
    for ok in ["a", "tone_of_voice", "a1", "x_2", &"x".repeat(64)] {
        assert!(valid_grade_axis_name(ok), "{ok} should be valid");
    }
    for bad in [
        "",
        "A",
        "1a",
        "_x",
        "tone of voice",
        "Tone",
        "töne",
        "a-b",
        &"x".repeat(65),
        "a\nb",
        "put_cost_usd<script>",
    ] {
        assert!(!valid_grade_axis_name(bad), "{bad:?} should be invalid");
    }
}

#[test]
fn label_pattern() {
    for ok in ["v1", "v3-tone-pass", "A_b-9", &"x".repeat(64)] {
        assert!(valid_label(ok), "{ok} should be valid");
    }
    for bad in ["", "has space", "a<b>", "café", &"x".repeat(65), "a\"b"] {
        assert!(!valid_label(bad), "{bad:?} should be invalid");
    }
}

#[test]
fn color_pattern() {
    for ok in ["#e07a5f", "#1F77B4", "#000000"] {
        assert!(valid_color(ok), "{ok} should be valid");
    }
    for bad in [
        "e07a5f", "#e07a5", "#e07a5ff", "#g07a5f", "#e07a5g", "", "# e07a5",
    ] {
        assert!(!valid_color(bad), "{bad:?} should be invalid");
    }
}

// ---------------------------------------------------------------------
// Grades PATCH validation
// ---------------------------------------------------------------------

#[test]
fn grades_patch_accepts_valid_and_null_delete() {
    let patch: GradesPatch =
        serde_json::from_str(r#"{"grades": {"tone": 0.8, "stale": null}}"#).unwrap();
    assert!(validate_grades_patch(&patch).is_ok());
    assert_eq!(patch.grades["tone"], Some(0.8));
    assert_eq!(patch.grades["stale"], None);
}

#[test]
fn grades_patch_rejects_reserved_axis() {
    let patch: GradesPatch = serde_json::from_str(r#"{"grades": {"put_cost_usd": 1.0}}"#).unwrap();
    let err = validate_grades_patch(&patch).unwrap_err();
    assert_eq!(err.problems[0].reason, "reserved_axis_name");
    assert!(
        err.problems[0].detail.contains("better: lower"),
        "detail names the direction"
    );
}

#[test]
fn grades_patch_collects_all_problems() {
    let patch: GradesPatch = serde_json::from_str(
        r#"{"grades": {"Bad Name": 1.0, "put_output_tokens": 2.0, "ok": 0.5}}"#,
    )
    .unwrap();
    let err = validate_grades_patch(&patch).unwrap_err();
    let reasons: Vec<_> = err.problems.iter().map(|p| p.reason).collect();
    assert_eq!(reasons, vec!["bad_axis_name", "reserved_axis_name"]);
}

#[test]
fn grades_patch_rejects_non_finite() {
    // serde_json already rejects out-of-range literals like 1e999 at
    // PARSE time ("number out of range"), so JSON can never smuggle an
    // infinity in; this check guards programmatic construction of a
    // GradesPatch (the struct is public).
    let patch = GradesPatch {
        grades: [("x".to_string(), Some(f64::INFINITY))]
            .into_iter()
            .collect(),
    };
    let err = validate_grades_patch(&patch).unwrap_err();
    assert_eq!(err.problems[0].reason, "non_finite_value");
}

// ---------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------

#[test]
fn investigations_parse_bare_and_detailed() {
    let req = parse_req(
        r##"{"investigations": ["a", {"id": "b", "label": "v2-warm", "color": "#e07a5f"}, {"id": "c"}],
             "axes": [{"name": "put_output_tokens", "better": "lower"}]}"##,
    );
    assert_eq!(req.investigations.len(), 3);
    assert_eq!(req.investigations[0].id(), "a");
    assert!(
        matches!(&req.investigations[1], FrontierInvestigation::Detailed { label, color, .. }
        if label.as_deref() == Some("v2-warm") && color.as_deref() == Some("#e07a5f"))
    );
    assert_eq!(req.axes[0].better, BetterDirection::Lower);
}

#[test]
fn better_direction_parses_lowercase_only() {
    assert!(serde_json::from_str::<FrontierAxis>(r#"{"name":"x","better":"lower"}"#).is_ok());
    assert!(serde_json::from_str::<FrontierAxis>(r#"{"name":"x","better":"higher"}"#).is_ok());
    assert!(serde_json::from_str::<FrontierAxis>(r#"{"name":"x","better":"up"}"#).is_err());
}

// ---------------------------------------------------------------------
// Reserved axis resolution
// ---------------------------------------------------------------------

#[test]
fn steps_stats_n1_is_zero_stdev() {
    let s = steps_stats(&[7]).unwrap();
    assert_eq!(s, (7.0, 7.0, 7.0, 0.0));
}

#[test]
fn steps_stats_population() {
    let s = steps_stats(&[2, 4]).unwrap();
    assert_eq!(s, (3.0, 2.0, 4.0, 1.0)); // stdev = sqrt(((2-3)^2+(4-3)^2)/2) = 1
    assert_eq!(steps_stats(&[3, 3, 3]).unwrap().3, 0.0);
    assert!(steps_stats(&[]).is_none());
}

#[test]
fn reserved_axes_resolve_from_usage_and_steps() {
    let mut s = snap("a");
    s.usage = Some(usage(1450));
    s.steps_per_trace = vec![2, 4, 6];
    assert_eq!(resolve_reserved(&s, "put_output_tokens"), Some(1450.0));
    assert_eq!(resolve_reserved(&s, "steps_per_trace_avg"), Some(4.0));
    assert_eq!(
        resolve_reserved(&s, "steps_per_trace_stdev"),
        Some(1.632993161855452)
    );
    // No completed traces → steps axes absent.
    s.steps_per_trace = vec![];
    assert_eq!(resolve_reserved(&s, "steps_per_trace_avg"), None);
    // Cost only when priced.
    assert_eq!(resolve_reserved(&s, "put_cost_usd"), None);
    s.usage = Some(usage(1450));
    s.usage.as_mut().unwrap().put.cost_usd = Some(0.42);
    assert_eq!(resolve_reserved(&s, "put_cost_usd"), Some(0.42));
}

// ---------------------------------------------------------------------
// compute: happy paths
// ---------------------------------------------------------------------

/// The demo campaign: 4 PUT variants graded on tone_of_voice with
/// measured put_output_tokens. Frontier = terse/warm/balanced; verbose
/// is dominated by two points.
fn demo_snapshots() -> BTreeMap<String, InvestigationSnapshot> {
    let mut m = BTreeMap::new();
    for (uuid, out, tone) in [
        ("11111111-terse", 1450u64, 0.4f64),
        ("22222222-warm", 2300, 0.85),
        ("33333333-balanced", 1800, 0.8),
        ("44444444-verbose", 3100, 0.75),
    ] {
        let mut s = snap(uuid);
        s.put_id = Some("cancel-bot".into());
        s.usage = Some(usage(out));
        s.grades.insert("tone_of_voice".into(), tone);
        m.insert(uuid.to_string(), s);
    }
    m
}

const DEMO_REQ: &str = r#"{
    "investigations": ["11111111-terse", "22222222-warm", "33333333-balanced",
                       {"id": "44444444-verbose", "label": "v4-verbose"}],
    "axes": [
        {"name": "put_output_tokens", "better": "lower"},
        {"name": "tone_of_voice", "better": "higher"}
    ]
}"#;

#[test]
fn compute_demo_campaign_dominance() {
    let snaps = demo_snapshots();
    let req = parse_req(DEMO_REQ);
    let res = compute(&req, &snaps, FrontierFormat::Svg).unwrap();
    let by: BTreeMap<&str, &FrontierPoint> = res
        .points
        .iter()
        .map(|p| (p.investigation.as_str(), p))
        .collect();

    // Mixed directions: cheapest tokens + best tone + balanced are all
    // non-dominated; verbose loses on both axes to warm and balanced.
    assert!(by["11111111-terse"].on_frontier);
    assert!(by["22222222-warm"].on_frontier);
    assert!(by["33333333-balanced"].on_frontier);
    assert!(!by["44444444-verbose"].on_frontier);
    let mut dom = by["44444444-verbose"].dominated_by.clone();
    dom.sort();
    assert_eq!(dom, vec!["22222222-warm", "33333333-balanced"]);

    // Default labels: put_id shared by all → deduplicated lineage names;
    // the explicit label passes through untouched.
    assert_eq!(by["11111111-terse"].label, "cancel-bot");
    assert_eq!(by["22222222-warm"].label, "cancel-bot#2");
    assert_eq!(by["33333333-balanced"].label, "cancel-bot#3");
    assert_eq!(by["44444444-verbose"].label, "v4-verbose");
    // Default colors from the palette by index; values resolve per axis.
    assert_eq!(by["11111111-terse"].color, "#1f77b4");
    assert_eq!(by["11111111-terse"].values["put_output_tokens"], 1450.0);
    assert_eq!(by["11111111-terse"].values["tone_of_voice"], 0.4);
}

#[test]
fn compute_ties_dominate_nothing() {
    let mut snaps = BTreeMap::new();
    for uuid in ["a", "b"] {
        let mut s = snap(uuid);
        s.grades.insert("tone".into(), 0.5);
        snaps.insert(uuid.into(), s);
    }
    let req = parse_req(
        r#"{"investigations": ["a", "b"], "axes": [{"name": "tone", "better": "higher"}]}"#,
    );
    let res = compute(&req, &snaps, FrontierFormat::Json).unwrap();
    assert!(
        res.points
            .iter()
            .all(|p| p.on_frontier && p.dominated_by.is_empty())
    );
}

#[test]
fn compute_n_axis_json() {
    // 3-axis dominance is legal for json (the 2-axis limit is svg-only).
    let mut snaps = BTreeMap::new();
    let mut s1 = snap("a");
    s1.grades.insert("tone".into(), 0.9);
    s1.usage = Some(usage(1000));
    s1.steps_per_trace = vec![5];
    let mut s2 = snap("b");
    s2.grades.insert("tone".into(), 0.5);
    s2.usage = Some(usage(1000));
    s2.steps_per_trace = vec![5];
    snaps.insert("a".into(), s1);
    snaps.insert("b".into(), s2);
    let req = parse_req(
        r#"{"investigations": ["a", "b"], "axes": [
            {"name": "tone", "better": "higher"},
            {"name": "put_output_tokens", "better": "lower"},
            {"name": "steps_per_trace_avg", "better": "lower"}]}"#,
    );
    let res = compute(&req, &snaps, FrontierFormat::Json).unwrap();
    assert!(res.points[0].on_frontier);
    assert!(!res.points[1].on_frontier);
    assert_eq!(res.points[1].dominated_by, vec!["a"]);
}

// ---------------------------------------------------------------------
// compute: every typed problem
// ---------------------------------------------------------------------

fn problems_of(
    req: &str,
    snaps: &BTreeMap<String, InvestigationSnapshot>,
    fmt: FrontierFormat,
) -> Vec<FrontierProblem> {
    compute(&parse_req(req), snaps, fmt).unwrap_err().problems
}

#[test]
fn problem_unknown_investigation() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["nope"], "axes": [{"name": "tone_of_voice", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "unknown_investigation");
    assert!(ps[0].detail.contains("lost on restart"));
}

#[test]
fn problem_duplicate_investigation() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse", "11111111-terse"],
            "axes": [{"name": "tone_of_voice", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "duplicate_investigation");
}

#[test]
fn problem_job_running_and_failed() {
    let mut snaps = demo_snapshots();
    let mut r = snap("running-job");
    r.status = SnapshotStatus::Running;
    snaps.insert("running-job".into(), r);
    let mut f = snap("failed-job");
    f.status = SnapshotStatus::Failed;
    snaps.insert("failed-job".into(), f);
    let ps = problems_of(
        r#"{"investigations": ["running-job", "failed-job"],
            "axes": [{"name": "tone_of_voice", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "job_running");
    assert!(ps[0].detail.contains("wait for status 'done'"));
    assert_eq!(ps[1].reason, "job_failed");
}

#[test]
fn problem_no_grade_names_the_patch_and_graded_axes() {
    let mut snaps = demo_snapshots();
    // A snapshot that graded `clarity` but NOT tone_of_voice.
    let mut s = snap("11111111-terse");
    s.put_id = Some("cancel-bot".into());
    s.grades.insert("clarity".into(), 1.0);
    snaps.insert("11111111-terse".into(), s);
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse"],
            "axes": [{"name": "tone_of_voice", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    let p = &ps[0];
    assert_eq!(p.reason, "no_grade");
    assert_eq!(p.axis.as_deref(), Some("tone_of_voice"));
    assert!(
        p.detail
            .contains("PATCH /api/investigations/11111111-terse"),
        "names the fix: {}",
        p.detail
    );
    assert!(p.detail.contains("\"tone_of_voice\": <number>"));
    assert!(
        p.detail
            .contains("graded axes on this investigation: clarity")
    );
}

#[test]
fn problem_axis_absent_unpriced_model_names_it() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse"],
            "axes": [{"name": "put_cost_usd", "better": "lower"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "axis_absent");
    assert!(ps[0].detail.contains("not priced in the model catalog"));
    assert!(
        ps[0].detail.contains("zai_coding::glm-5.2"),
        "names the model"
    );
}

#[test]
fn problem_direction_conflict() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse"],
            "axes": [{"name": "put_output_tokens", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "direction_conflict");
    assert!(ps[0].detail.contains("better: lower"));
}

#[test]
fn problem_bad_axis_name_lists_reserved_vocabulary() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse"],
            "axes": [{"name": "Tone!!", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    assert_eq!(ps[0].reason, "bad_axis_name");
    assert!(ps[0].detail.contains("steps_per_trace_{avg,min,max,stdev}"));
}

#[test]
fn problem_duplicate_axis_and_arity_and_empty() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": ["11111111-terse"], "axes": [
            {"name": "tone_of_voice", "better": "higher"},
            {"name": "tone_of_voice", "better": "higher"},
            {"name": "put_output_tokens", "better": "lower"}]}"#,
        &snaps,
        FrontierFormat::Svg, // svg with 3 axes (incl. a dup)
    );
    let reasons: Vec<&str> = ps.iter().map(|p| p.reason).collect();
    assert!(reasons.contains(&"duplicate_axis"));
    assert!(reasons.contains(&"axis_arity"));

    let ps = problems_of(
        r#"{"investigations": [], "axes": []}"#,
        &snaps,
        FrontierFormat::Json,
    );
    let reasons: Vec<&str> = ps.iter().map(|p| p.reason).collect();
    assert_eq!(reasons, vec!["empty_investigations", "empty_axes"]);
}

#[test]
fn problem_bad_label_and_color() {
    let snaps = demo_snapshots();
    let ps = problems_of(
        r#"{"investigations": [{"id": "11111111-terse", "label": "bad <script>", "color": "red"}],
            "axes": [{"name": "tone_of_voice", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    let reasons: Vec<&str> = ps.iter().map(|p| p.reason).collect();
    assert_eq!(reasons, vec!["bad_label", "bad_color"]);
}

#[test]
fn problems_are_collected_not_fail_fast() {
    // One request, several distinct fixable problems, all reported.
    let mut snaps = demo_snapshots();
    let mut r = snap("running-job");
    r.status = SnapshotStatus::Running;
    snaps.insert("running-job".into(), r);
    let ps = problems_of(
        r#"{"investigations": ["running-job", "ghost", "11111111-terse"],
            "axes": [{"name": "tone_of_voice", "better": "higher"},
                     {"name": "put_cost_usd", "better": "higher"}]}"#,
        &snaps,
        FrontierFormat::Json,
    );
    let mut reasons: Vec<&str> = ps.iter().map(|p| p.reason).collect();
    reasons.sort_unstable(); // ordering is not contractual; membership is
    let mut want = vec![
        "axis_absent",
        "direction_conflict",
        "job_running",
        "unknown_investigation",
    ];
    want.sort_unstable();
    assert_eq!(reasons, want);
}

// ---------------------------------------------------------------------
// SVG golden
// ---------------------------------------------------------------------

const GOLDEN: &str = include_str!("testdata/frontier_golden.svg");

fn demo_svg() -> String {
    let snaps = demo_snapshots();
    let req = parse_req(DEMO_REQ);
    let res = compute(&req, &snaps, FrontierFormat::Svg).unwrap();
    svg::render(
        &res.points,
        &svg::PlotAxis::new("put_output_tokens", BetterDirection::Lower),
        &svg::PlotAxis::new("tone_of_voice", BetterDirection::Higher),
    )
}

#[test]
fn svg_golden_deterministic() {
    let out = demo_svg();
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/frontier/testdata/frontier_golden.svg"
        );
        std::fs::write(path, &out).unwrap();
        eprintln!("golden updated");
        return;
    }
    assert_eq!(
        out, GOLDEN,
        "SVG changed — inspect and re-run with UPDATE_GOLDEN=1"
    );
}

#[test]
fn svg_renders_single_point_without_panic() {
    let mut snaps = BTreeMap::new();
    let mut s = snap("only");
    s.grades.insert("tone".into(), 0.7);
    s.usage = Some(usage(42));
    snaps.insert("only".into(), s);
    let req = parse_req(
        r#"{"investigations": ["only"], "axes": [
            {"name": "tone", "better": "higher"},
            {"name": "put_output_tokens", "better": "lower"}]}"#,
    );
    let res = compute(&req, &snaps, FrontierFormat::Svg).unwrap();
    let out = svg::render(
        &res.points,
        &svg::PlotAxis::new("tone", BetterDirection::Higher),
        &svg::PlotAxis::new("put_output_tokens", BetterDirection::Lower),
    );
    assert!(out.contains("<svg"));
    assert!(out.contains("only"));
}

#[test]
fn svg_handles_degenerate_same_value_points() {
    let mut snaps = BTreeMap::new();
    for uuid in ["a", "b"] {
        let mut s = snap(uuid);
        s.grades.insert("tone".into(), 0.5); // identical values everywhere
        snaps.insert(uuid.into(), s);
    }
    let req = parse_req(
        r#"{"investigations": ["a", "b"], "axes": [
            {"name": "tone", "better": "higher"},
            {"name": "steps_per_trace_avg", "better": "lower"}]}"#,
    );
    let res = compute(&req, &snaps, FrontierFormat::Svg).unwrap();
    let out = svg::render(
        &res.points,
        &svg::PlotAxis::new("tone", BetterDirection::Higher),
        &svg::PlotAxis::new("steps_per_trace_avg", BetterDirection::Lower),
    );
    assert!(out.contains("<path"));
}
