//! Multi-dimensional prompt optimization: caller-graded axes and the
//! Pareto frontier over them.
//!
//! The caller is the judge — that does not change here. Grades are the
//! caller's judgment RECORDED (stored, merged, never interpreted); the
//! frontier is deterministic arithmetic over caller-supplied numbers,
//! the same class of work as diffs and budget counting.
//!
//! Two kinds of axes:
//! - **Reserved (measured)** axes — computed from run data (token
//!   usage, estimated cost, steps-per-trace statistics). Their
//!   better-direction is baked in. They can never be PATCHed.
//! - **Graded (judged)** axes — caller-PATCHed scalars on free-form
//!   names. Direction is supplied per REQUEST (`better`), never
//!   stored: direction only matters at dominance/plot time, and
//!   storing it would create axis-registration machinery for zero
//!   gain.
//!
//! Dominance is N-dimensional from day one; the 2-axis limit is a
//! v0 RENDERING constraint enforced only for `format=svg`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm::track::UsageByRole;

/// The full reserved (measured) axis vocabulary with baked-in
/// directions. Named `<metric>_<statistic>`/`<role>_<metric>` so
/// future per-trace normalizations slot in without renames.
pub const RESERVED_AXES: &[(&str, BetterDirection)] = &[
    ("put_input_tokens", BetterDirection::Lower),
    ("put_output_tokens", BetterDirection::Lower),
    ("put_cache_read_tokens", BetterDirection::Higher),
    ("put_cost_usd", BetterDirection::Lower),
    ("sim_input_tokens", BetterDirection::Lower),
    ("sim_output_tokens", BetterDirection::Lower),
    ("sim_cache_read_tokens", BetterDirection::Higher),
    ("sim_cost_usd", BetterDirection::Lower),
    ("steps_per_trace_avg", BetterDirection::Lower),
    ("steps_per_trace_min", BetterDirection::Lower),
    ("steps_per_trace_max", BetterDirection::Lower),
    ("steps_per_trace_stdev", BetterDirection::Lower),
];

/// A compact rendering of the reserved vocabulary for error details
/// (typo detection: callers can scan it for the name they meant).
pub const RESERVED_AXES_COMPACT: &str = "put_/sim_input_tokens, put_/sim_output_tokens, \
     put_/sim_cache_read_tokens, put_/sim_cost_usd, steps_per_trace_{avg,min,max,stdev}";

/// The categorical palette for default point colors (deterministic by
/// investigation index; caller-supplied colors override).
pub const PALETTE: &[&str] = &[
    "#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#7f7f7f",
    "#bcbd22", "#17becf",
];

/// Whether lower or higher values are better on an axis. Supplied by
/// the caller per request for graded axes; baked in for reserved ones.
/// Dominance normalizes internally (negating lower-is-better values),
/// so "higher score = better" uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BetterDirection {
    Lower,
    Higher,
}

impl BetterDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            BetterDirection::Lower => "lower",
            BetterDirection::Higher => "higher",
        }
    }
}

/// Direction of a reserved (measured) axis, if `name` is one.
pub fn reserved_direction(name: &str) -> Option<BetterDirection> {
    RESERVED_AXES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, d)| *d)
}

/// Graded-axis allow-pattern, enforced WITHOUT a regex dependency:
/// `^[a-z][a-z0-9_]{0,63}$` — starts lowercase, then up to 63 of
/// [a-z0-9_]. An ALLOW-pattern (never a deny-list) so exotic input
/// cannot smuggle anything through.
pub fn valid_grade_axis_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest = chars.count();
    rest <= 63
        && name[1..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Plot label allow-pattern, regex-free: `^[A-Za-z0-9_-]{1,64}$`.
pub fn valid_label(label: &str) -> bool {
    let n = label.chars().count();
    (1..=64).contains(&n)
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Color allow-pattern, regex-free: `^#[0-9a-fA-F]{6}$`.
pub fn valid_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// PATCH /api/investigations/{id}
// ---------------------------------------------------------------------------

/// Caller judgment recorded on an investigation: axis name → number.
/// Merge semantics per axis: a number sets/overwrites, `null` deletes.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct GradesPatch {
    /// Axis name → value. Use JSON `null` to DELETE an axis. Axis
    /// names must match `^[a-z][a-z0-9_]{0,63}$` and must not collide
    /// with a reserved measured axis (see the frontier docs).
    pub grades: BTreeMap<String, Option<f64>>,
}

/// The echo response: the full, updated grades map.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GradesView {
    pub grades: BTreeMap<String, f64>,
}

/// One fixable problem in a grades PATCH. The `detail` names the fix.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GradeProblem {
    pub axis: String,
    /// `bad_axis_name` | `reserved_axis_name` | `non_finite_value`
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GradesPatchError {
    pub error: &'static str,
    pub problems: Vec<GradeProblem>,
}

/// Validate a grades PATCH: every axis name must be a legal graded-axis
/// name (allow-pattern), must not collide with a reserved measured
/// axis, and every value must be finite. Collects ALL problems (not
/// fail-fast) so the caller can fix everything in one round-trip.
pub fn validate_grades_patch(patch: &GradesPatch) -> Result<(), GradesPatchError> {
    let mut problems = Vec::new();
    for (axis, value) in &patch.grades {
        if let Some(dir) = reserved_direction(axis) {
            problems.push(GradeProblem {
                axis: axis.clone(),
                reason: "reserved_axis_name",
                detail: format!(
                    "'{axis}' is a reserved measured axis (better: {}); measured axes are \
                     computed by the harness and cannot be graded — pick a different name",
                    dir.as_str()
                ),
            });
            continue;
        }
        if !valid_grade_axis_name(axis) {
            problems.push(GradeProblem {
                axis: axis.clone(),
                reason: "bad_axis_name",
                detail: format!(
                    "axis name '{axis}' fails the allow-pattern ^[a-z][a-z0-9_]{{0,63}}$ \
                     (lowercase, then lowercase/digits/underscore, ≤ 64 chars)"
                ),
            });
            continue;
        }
        if let Some(v) = value {
            // NaN/Infinity are not expressible as JSON literals, but
            // exponents like 1e999 parse to infinity — reject explicitly.
            if !v.is_finite() {
                problems.push(GradeProblem {
                    axis: axis.clone(),
                    reason: "non_finite_value",
                    detail: format!(
                        "value for '{axis}' is not a finite number — grades must be finite \
                         JSON numbers (NaN/Infinity are not JSON)"
                    ),
                });
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(GradesPatchError {
            error: "grades_invalid",
            problems,
        })
    }
}

// ---------------------------------------------------------------------------
// Frontier request / response
// ---------------------------------------------------------------------------

/// An investigation referenced by the frontier request: a bare id, or
/// an object carrying an optional plot label and color.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum FrontierInvestigation {
    Id(String),
    Detailed {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
    },
}

impl FrontierInvestigation {
    pub fn id(&self) -> &str {
        match self {
            FrontierInvestigation::Id(id) => id,
            FrontierInvestigation::Detailed { id, .. } => id,
        }
    }
}

/// One axis of the frontier plot, with the caller's direction.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct FrontierAxis {
    /// A graded axis name (you PATCHed it) or a reserved measured axis
    /// (harness-computed). Reserved names and their baked-in
    /// directions: put_/sim_input_tokens (lower), put_/sim_output_tokens
    /// (lower), put_/sim_cache_read_tokens (higher — cached input is
    /// cheaper input), put_/sim_cost_usd (lower), sim_cost_usd (lower),
    /// steps_per_trace_avg/_min/_max/_stdev (lower). Requesting a
    /// reserved axis with a contradicting `better` is rejected.
    pub name: String,
    /// Whether lower or higher values are better on this axis. For
    /// graded axes this is YOUR call (encode direction in your own
    /// scale, e.g. grade "repeatability" high-good rather than
    /// "variance" low-good); for reserved axes it must match the
    /// measured direction.
    pub better: BetterDirection,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct FrontierRequest {
    /// The investigations to plot (each must be a `done` job with a
    /// value for every axis). Bare id strings or `{id, label?, color?}`
    /// objects. Ids must be UNIQUE — duplicates are rejected. Labels:
    /// `^[[A-Za-z0-9_-]{1,64}$`; colors: `#rrggbb`. Defaults: label =
    /// the PUT's id (deduplicated) else the uuid prefix; color = a
    /// deterministic palette by position.
    pub investigations: Vec<FrontierInvestigation>,
    /// The axes to compute dominance over. `format=svg` requires
    /// EXACTLY 2 (a v0 rendering constraint — the dominance math is
    /// N-dimensional); `format=json` accepts any count ≥ 1.
    pub axes: Vec<FrontierAxis>,
}

/// One point of the frontier result.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FrontierPoint {
    /// The investigation uuid (uuids, not labels, are the stable key —
    /// labels are not unique by design).
    pub investigation: String,
    pub label: String,
    pub color: String,
    /// Resolved value per axis name.
    pub values: BTreeMap<String, f64>,
    /// True when no other point dominates this one. Ties dominate
    /// nothing: equal points are both on the frontier.
    pub on_frontier: bool,
    /// Investigations that dominate this point (empty when on the
    /// frontier). Tells an optimizer exactly what to compare against.
    pub dominated_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FrontierResponse {
    pub points: Vec<FrontierPoint>,
}

/// One fixable problem in a frontier request. Every `detail` names the
/// fix — including, for missing grades, the exact PATCH to make.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FrontierProblem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub investigation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FrontierError {
    pub error: &'static str,
    pub problems: Vec<FrontierProblem>,
}

// ---------------------------------------------------------------------------
// Snapshots: everything the frontier needs from one investigation
// ---------------------------------------------------------------------------

/// Job lifecycle as the frontier sees it. Mirrors the server's job
/// status; core-side so `compute` is testable standalone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStatus {
    Running,
    Done,
    Failed,
}

/// The harness-side facts about one investigation, assembled by the
/// server from its job store. Pure data; `compute` does the rest.
#[derive(Debug, Clone)]
pub struct InvestigationSnapshot {
    pub id: String,
    pub status: SnapshotStatus,
    /// The PUT's caller-supplied id, used as the default plot label.
    /// None/empty falls back to the uuid prefix.
    pub put_id: Option<String>,
    /// Caller-graded axes (PATCHed). Never interpreted.
    pub grades: BTreeMap<String, f64>,
    /// Token usage split by role; Some once the run finished.
    pub usage: Option<UsageByRole>,
    /// Model names (for error details: unpriced-model messages).
    pub put_model: Option<String>,
    pub sim_model: Option<String>,
    /// Per-trace step counts (completed attempts only). A "step" is
    /// one tool call OR one final completion — the same unit the
    /// `max_steps_per_trace` budget counts.
    pub steps_per_trace: Vec<u64>,
}

fn steps_stats(steps: &[u64]) -> Option<(f64, f64, f64, f64)> {
    if steps.is_empty() {
        return None;
    }
    let n = steps.len() as f64;
    let min = *steps.iter().min().unwrap() as f64;
    let max = *steps.iter().max().unwrap() as f64;
    let avg = steps.iter().sum::<u64>() as f64 / n;
    // Population stdev (÷N): well-defined at N=1 (→ 0.0) and describes
    // THIS corpus rather than estimating a population.
    let var = steps
        .iter()
        .map(|&s| {
            let d = s as f64 - avg;
            d * d
        })
        .sum::<f64>()
        / n;
    Some((avg, min, max, var.sqrt()))
}

/// Resolve a reserved axis name against a snapshot. None means "no
/// value for this axis on this investigation" (the caller gets an
/// `axis_absent` problem explaining why).
fn resolve_reserved(snapshot: &InvestigationSnapshot, axis: &str) -> Option<f64> {
    let usage = snapshot.usage?;
    match axis {
        "put_input_tokens" => Some(usage.put.input_tokens as f64),
        "put_output_tokens" => Some(usage.put.output_tokens as f64),
        "put_cache_read_tokens" => Some(usage.put.cache_read_tokens as f64),
        "put_cost_usd" => usage.put.cost_usd,
        "sim_input_tokens" => Some(usage.sim.input_tokens as f64),
        "sim_output_tokens" => Some(usage.sim.output_tokens as f64),
        "sim_cache_read_tokens" => Some(usage.sim.cache_read_tokens as f64),
        "sim_cost_usd" => usage.sim.cost_usd,
        "steps_per_trace_avg" => steps_stats(&snapshot.steps_per_trace).map(|s| s.0),
        "steps_per_trace_min" => steps_stats(&snapshot.steps_per_trace).map(|s| s.1),
        "steps_per_trace_max" => steps_stats(&snapshot.steps_per_trace).map(|s| s.2),
        "steps_per_trace_stdev" => steps_stats(&snapshot.steps_per_trace).map(|s| s.3),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Compute
// ---------------------------------------------------------------------------

/// Which serialization the frontier endpoint was asked for. The 2-axis
/// limit is enforced only for `Svg` — a v0 RENDERING constraint, not a
/// data-model one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierFormat {
    Json,
    Svg,
}

/// Validate the request against the snapshots, resolve every
/// (investigation, axis) value, and compute Pareto dominance.
///
/// Collects ALL fixable problems into one typed 422-shaped error
/// (negative results are first-class); fails only when problem-free.
pub fn compute(
    req: &FrontierRequest,
    snapshots: &BTreeMap<String, InvestigationSnapshot>,
    format: FrontierFormat,
) -> Result<FrontierResponse, FrontierError> {
    let mut problems: Vec<FrontierProblem> = Vec::new();

    if req.investigations.is_empty() {
        problems.push(FrontierProblem {
            investigation: None,
            axis: None,
            reason: "empty_investigations",
            detail: "'investigations' is empty — list at least one investigation id \
                     (from POST /api/investigations)"
                .into(),
        });
    }
    if req.axes.is_empty() {
        problems.push(FrontierProblem {
            investigation: None,
            axis: None,
            reason: "empty_axes",
            detail: "'axes' is empty — list at least one axis (graded names you PATCHed, \
                     or reserved measured names)"
                .into(),
        });
    }
    if format == FrontierFormat::Svg && !req.axes.is_empty() && req.axes.len() != 2 {
        problems.push(FrontierProblem {
            investigation: None,
            axis: None,
            reason: "axis_arity",
            detail: format!(
                "format=svg plots exactly 2 axes (got {}); the dominance computation is \
                 N-dimensional — send 2 axes for a plot, or format=json for {} axes",
                req.axes.len(),
                req.axes.len()
            ),
        });
    }

    // Duplicate investigation ids (explicit caller decision: enforce).
    let mut seen_ids: Vec<&str> = Vec::new();
    for inv in &req.investigations {
        let id = inv.id();
        if seen_ids.contains(&id) {
            problems.push(FrontierProblem {
                investigation: Some(id.to_string()),
                axis: None,
                reason: "duplicate_investigation",
                detail: format!(
                    "investigation '{id}' appears more than once — points are identified \
                     by uuid; remove the duplicates"
                ),
            });
        } else {
            seen_ids.push(id);
        }
    }

    // Label / color allow-patterns (injection defense, enforced BEFORE
    // anything reaches the renderer).
    for inv in &req.investigations {
        if let FrontierInvestigation::Detailed { id, label, color } = inv {
            if let Some(l) = label.as_deref().filter(|l| !valid_label(l)) {
                problems.push(FrontierProblem {
                    investigation: Some(id.clone()),
                    axis: None,
                    reason: "bad_label",
                    detail: format!(
                        "label '{l}' fails the allow-pattern ^[A-Za-z0-9_-]{{1,64}}$ \
                         (letters, digits, dash, underscore — no spaces or markup)"
                    ),
                });
            }
            if let Some(c) = color.as_deref().filter(|c| !valid_color(c)) {
                problems.push(FrontierProblem {
                    investigation: Some(id.clone()),
                    axis: None,
                    reason: "bad_color",
                    detail: format!("color '{c}' must be 6-digit hex like '#e07a5f'"),
                });
            }
        }
    }

    // Axis names: reserved, or matching the graded allow-pattern; and
    // unique. Direction conflicts with reserved directions are caught
    // per axis here (not per investigation).
    let mut seen_axes: Vec<&str> = Vec::new();
    for axis in &req.axes {
        if seen_axes.contains(&axis.name.as_str()) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(axis.name.clone()),
                reason: "duplicate_axis",
                detail: format!(
                    "axis '{}' appears more than once — each axis is plotted once",
                    axis.name
                ),
            });
        } else {
            seen_axes.push(&axis.name);
        }
        if let Some(reserved) = reserved_direction(&axis.name) {
            if axis.better != reserved {
                problems.push(FrontierProblem {
                    investigation: None,
                    axis: Some(axis.name.clone()),
                    reason: "direction_conflict",
                    detail: format!(
                        "axis '{}' is reserved and measured better: {} — set 'better' to \
                         \"{}\" or drop the axis",
                        axis.name,
                        reserved.as_str(),
                        reserved.as_str()
                    ),
                });
            }
        } else if !valid_grade_axis_name(&axis.name) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(axis.name.clone()),
                reason: "bad_axis_name",
                detail: format!(
                    "axis name '{}' is neither reserved (measured: {}) nor a valid graded \
                     name (pattern ^[a-z][a-z0-9_]{{0,63}}$ — is it a name you PATCHed?)",
                    axis.name, RESERVED_AXES_COMPACT
                ),
            });
        }
    }

    // Default labels for bare-id entries, deduplicated among themselves
    // (a second variant of the same PUT lineage becomes "name#2").
    // Computed BEFORE the main loop so counting is single-pass.
    let mut default_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut default_labels: Vec<Option<String>> = Vec::with_capacity(req.investigations.len());
    for inv in &req.investigations {
        if matches!(inv, FrontierInvestigation::Detailed { .. }) {
            default_labels.push(None);
            continue;
        }
        let base = snapshots
            .get(inv.id())
            .and_then(|s| s.put_id.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| inv.id().chars().take(8).collect());
        let n = default_counts.entry(base.clone()).or_insert(0);
        *n += 1;
        default_labels.push(Some(if *n == 1 { base } else { format!("{base}#{n}") }));
    }

    // Resolve values. Status problems are per-investigation (the root
    // cause); value problems are per (investigation, axis).
    let mut rows: Vec<(&InvestigationSnapshot, Vec<f64>, String, String)> = Vec::new();
    for (idx, inv) in req.investigations.iter().enumerate() {
        let id = inv.id();
        let Some(snap) = snapshots.get(id) else {
            problems.push(FrontierProblem {
                investigation: Some(id.to_string()),
                axis: None,
                reason: "unknown_investigation",
                detail: format!(
                    "no investigation '{id}' in this server's memory — ids come from POST \
                     /api/investigations and are lost on restart; check the id or re-run"
                ),
            });
            continue;
        };
        if snap.status != SnapshotStatus::Done {
            let (reason, remedy) = match snap.status {
                SnapshotStatus::Running => (
                    "job_running",
                    "wait for status 'done' (poll GET /api/investigations/{id}) or drop this id",
                ),
                SnapshotStatus::Failed => (
                    "job_failed",
                    "it has no judgeable traces or complete usage — drop this id or re-run it",
                ),
                SnapshotStatus::Done => unreachable!(),
            };
            problems.push(FrontierProblem {
                investigation: Some(id.to_string()),
                axis: None,
                reason,
                detail: format!(
                    "investigation '{id}' is {} — {remedy}",
                    match snap.status {
                        SnapshotStatus::Running => "still running",
                        SnapshotStatus::Failed => "failed",
                        SnapshotStatus::Done => "done",
                    }
                ),
            });
            continue;
        }

        let mut values = Vec::with_capacity(req.axes.len());
        for axis in &req.axes {
            if let Some(_reserved) = reserved_direction(&axis.name) {
                match resolve_reserved(snap, &axis.name) {
                    Some(v) => values.push(v),
                    None => {
                        let why = if axis.name.ends_with("_cost_usd") {
                            let role_model = if axis.name.starts_with("put_") {
                                snap.put_model.as_deref()
                            } else {
                                snap.sim_model.as_deref()
                            };
                            format!(
                                "the {} model '{}' is not priced in the model catalog, so \
                                 cost cannot be measured — use a token axis instead, grade \
                                 cost yourself as a judged axis, or drop this investigation",
                                axis.name.split('_').next().unwrap_or("model"),
                                role_model.unwrap_or("(unknown)")
                            )
                        } else {
                            "no completed traces on this investigation — drop it or re-run"
                                .to_string()
                        };
                        problems.push(FrontierProblem {
                            investigation: Some(id.to_string()),
                            axis: Some(axis.name.clone()),
                            reason: "axis_absent",
                            detail: format!(
                                "axis '{}' has no value on investigation '{id}': {why}",
                                axis.name
                            ),
                        });
                        values.push(f64::NAN); // placeholder; error path wins anyway
                    }
                }
            } else {
                match snap.grades.get(&axis.name) {
                    Some(v) => values.push(*v),
                    None => {
                        let graded: Vec<&str> = snap.grades.keys().map(|s| s.as_str()).collect();
                        problems.push(FrontierProblem {
                            investigation: Some(id.to_string()),
                            axis: Some(axis.name.clone()),
                            reason: "no_grade",
                            detail: format!(
                                "no caller grade named '{}' on investigation '{id}'; PATCH \
                                 /api/investigations/{id} with {{\"grades\":{{\"{}\": <number>}}}} \
                                 (higher = better on your scale, per this request's 'better'); \
                                 graded axes on this investigation: {}; reserved measured axes: {}",
                                axis.name,
                                axis.name,
                                if graded.is_empty() {
                                    "(none)".to_string()
                                } else {
                                    graded.join(", ")
                                },
                                RESERVED_AXES_COMPACT
                            ),
                        });
                        values.push(f64::NAN);
                    }
                }
            }
        }

        // Label/color resolution: caller-supplied or defaulted above.
        let (label, color) = match inv {
            FrontierInvestigation::Id(_) => (
                default_labels[idx]
                    .clone()
                    .unwrap_or_else(|| id.chars().take(8).collect()),
                PALETTE[idx % PALETTE.len()].to_string(),
            ),
            FrontierInvestigation::Detailed { label, color, .. } => (
                label
                    .clone()
                    .unwrap_or_else(|| id.chars().take(8).collect()),
                color
                    .clone()
                    .unwrap_or_else(|| PALETTE[idx % PALETTE.len()].to_string()),
            ),
        };
        rows.push((snap, values, label, color));
    }

    if !problems.is_empty() {
        return Err(FrontierError {
            error: "frontier_request_invalid",
            problems,
        });
    }

    // Normalize: on every axis, higher normalized score = better.
    let dirs: Vec<f64> = req
        .axes
        .iter()
        .map(|a| {
            if a.better == BetterDirection::Higher {
                1.0
            } else {
                -1.0
            }
        })
        .collect();
    let scores: Vec<Vec<f64>> = rows
        .iter()
        .map(|(_, values, _, _)| values.iter().zip(&dirs).map(|(v, d)| v * d).collect())
        .collect();

    let ids: Vec<&str> = rows.iter().map(|(s, _, _, _)| s.id.as_str()).collect();
    let mut points = Vec::with_capacity(rows.len());
    for (i, (_, values, label, color)) in rows.iter().enumerate() {
        let mut dominated_by = Vec::new();
        let mut on_frontier = true;
        for (j, _) in rows.iter().enumerate() {
            if i == j {
                continue;
            }
            let ge = scores[j].iter().zip(&scores[i]).all(|(a, b)| a >= b);
            let gt = scores[j].iter().zip(&scores[i]).any(|(a, b)| a > b);
            if ge && gt {
                dominated_by.push(ids[j].to_string());
                on_frontier = false;
            }
        }
        points.push(FrontierPoint {
            investigation: ids[i].to_string(),
            label: label.clone(),
            color: color.clone(),
            values: req
                .axes
                .iter()
                .map(|a| a.name.clone())
                .zip(values.iter().cloned())
                .collect(),
            on_frontier,
            dominated_by,
        });
    }

    Ok(FrontierResponse { points })
}

pub mod svg;

#[cfg(test)]
mod tests;
