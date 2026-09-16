//! Grouped Pareto frontiers.  Unlike the legacy frontier, the selection is
//! the complete snapshot map: group tags decide which runs are summarized.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::tags::{canonical_group_tags, stable_hash_hex, valid_tag_name};

/// SVG rendering companion for this grouped response shape.
pub use super::svg::render_grouped;
use super::{
    BetterDirection, FrontierAxis, FrontierError, FrontierFormat, FrontierProblem,
    InvestigationSnapshot, PALETTE, RESERVED_AXES_COMPACT, SnapshotStatus, reserved_direction,
    resolve_reserved, valid_grade_axis_name,
};

/// One snapshot plus the tags by which it may be grouped.  A missing tag is
/// meaningful: it belongs to that grouping key's explicit `null` bucket.
#[derive(Debug, Clone)]
pub struct GroupedSnapshot {
    pub snapshot: InvestigationSnapshot,
    pub tags: BTreeMap<String, String>,
}

/// Request a frontier over means of complete investigations in each tag group.
/// There is intentionally no investigation selection field: accepting an old
/// selection accidentally as an empty selection would silently mean all jobs.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupedFrontierRequest {
    /// Tag names that form a group. Omit for exactly
    /// `["put_model", "put_thinking", "prompt_hash"]`; send `[]` for one
    /// group containing every current job. A job missing a requested key is
    /// retained in that key's explicit JSON-null group, never dropped.
    #[serde(default = "default_group_by")]
    pub group_by: Vec<String>,
    /// Axes whose arithmetic means define Pareto dominance. Every included run
    /// has every requested value, so different axes never average different
    /// cohorts.
    pub axes: Vec<FrontierAxis>,
}

fn default_group_by() -> Vec<String> {
    vec![
        "put_model".into(),
        "put_thinking".into(),
        "prompt_hash".into(),
    ]
}

/// One omitted run and why it is not part of its group's common cohort.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupExclusion {
    pub investigation: String,
    /// `running`, `failed`, `awaiting_grades`, or `unavailable`.
    pub status: String,
    /// All requested caller-graded axes absent from this run. This remains
    /// populated for running and failed jobs to make the grading backlog seen.
    pub missing_grades: Vec<String>,
    /// Requested measured axes with no value (for example an unpriced cost).
    pub missing_axes: Vec<String>,
}

/// One stable tag group. Groups without usable runs are deliberately retained
/// with null values/frontier state rather than disappearing from the result.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupedFrontierPoint {
    /// Stable SHA-256-derived id of canonical grouping tags only; membership
    /// changes do not recolor or rename a group.
    pub id: String,
    /// The requested group tags. Missing source values appear as JSON null.
    pub tags: BTreeMap<String, Option<String>>,
    /// Human-readable summary of grouping tags, not an editable group identity.
    /// The investigation's editable `label` tag names its UI card; it affects
    /// grouping only when explicitly selected in `group_by`.
    pub label: String,
    /// Stable categorical color derived from this group's id.
    pub color: String,
    /// All snapshot ids in this group, sorted.
    pub investigations: Vec<String>,
    /// Investigation ids used in every mean, sorted. Each has equal weight;
    /// all requested axes use exactly this same completed, fully-valued cohort.
    pub included: Vec<String>,
    pub excluded: Vec<GroupExclusion>,
    /// True when any member was excluded. Preliminary points still participate
    /// in dominance when they have a complete common cohort.
    pub preliminary: bool,
    /// Axis → arithmetic mean, or null if no member has all requested values.
    /// Always present, even for pending groups (null means no coordinates).
    #[schema(required = true)]
    pub values: Option<BTreeMap<String, f64>>,
    /// True if non-dominated, false if dominated, null if pending (no values).
    /// Preliminary points with values participate in the current frontier.
    #[schema(required = true)]
    pub on_frontier: Option<bool>,
    /// IDs of dominating GROUPS, not investigation ids or display labels.
    /// Empty for non-dominated and pending groups; equal means do not dominate.
    pub dominated_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupedFrontierResponse {
    pub points: Vec<GroupedFrontierPoint>,
}

/// Validate and compute grouped arithmetic means and Pareto dominance.
/// Lifecycle, missing grades, and unavailable measured values are evidence in
/// the response, not request errors. Only malformed axes/group names fail.
pub fn compute_grouped(
    req: &GroupedFrontierRequest,
    snapshots: &BTreeMap<String, GroupedSnapshot>,
    format: FrontierFormat,
) -> Result<GroupedFrontierResponse, FrontierError> {
    let mut problems = Vec::new();
    if req.axes.is_empty() {
        problems.push(FrontierProblem {
            investigation: None,
            axis: None,
            reason: "empty_axes",
            detail: "'axes' is empty — list at least one graded or reserved measured axis".into(),
        });
    }
    if format == FrontierFormat::Svg && !req.axes.is_empty() && req.axes.len() != 2 {
        problems.push(FrontierProblem {
            investigation: None,
            axis: None,
            reason: "axis_arity",
            detail: format!(
                "format=svg plots exactly 2 axes (got {}); use two axes or format=json",
                req.axes.len()
            ),
        });
    }
    let mut seen_tags = BTreeSet::new();
    for tag in &req.group_by {
        if !seen_tags.insert(tag) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(tag.clone()),
                reason: "duplicate_group_tag",
                detail: format!(
                    "group tag '{tag}' appears more than once — list each grouping key once"
                ),
            });
        }
        if !valid_tag_name(tag) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(tag.clone()),
                reason: "bad_group_tag",
                detail: format!("group tag '{tag}' fails ^[a-z][a-z0-9_]{{0,63}}$"),
            });
        }
    }
    let mut seen_axes = BTreeSet::new();
    for axis in &req.axes {
        if !seen_axes.insert(&axis.name) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(axis.name.clone()),
                reason: "duplicate_axis",
                detail: format!(
                    "axis '{}' appears more than once — each axis is plotted once",
                    axis.name
                ),
            });
        }
        if let Some(dir) = reserved_direction(&axis.name) {
            if axis.better != dir {
                problems.push(FrontierProblem {
                    investigation: None,
                    axis: Some(axis.name.clone()),
                    reason: "direction_conflict",
                    detail: format!(
                        "axis '{}' is reserved and measured better: {}",
                        axis.name,
                        dir.as_str()
                    ),
                });
            }
        } else if !valid_grade_axis_name(&axis.name) {
            problems.push(FrontierProblem {
                investigation: None,
                axis: Some(axis.name.clone()),
                reason: "bad_axis_name",
                detail: format!(
                    "axis '{}' is neither reserved ({}) nor a valid graded name",
                    axis.name, RESERVED_AXES_COMPACT
                ),
            });
        }
    }
    if !problems.is_empty() {
        return Err(FrontierError {
            error: "frontier_request_invalid",
            problems,
        });
    }

    // Canonical group tags are the map key, making output ordering and all
    // group identity independent of insertion order and member ids.
    let mut groups: BTreeMap<Vec<(String, Option<String>)>, Vec<&GroupedSnapshot>> =
        BTreeMap::new();
    for grouped in snapshots.values() {
        let tags = req
            .group_by
            .iter()
            .map(|key| (key.clone(), grouped.tags.get(key).cloned()))
            .collect();
        groups.entry(tags).or_default().push(grouped);
    }

    let mut points = Vec::with_capacity(groups.len());
    for (tag_pairs, members) in groups {
        let tags: BTreeMap<String, Option<String>> = tag_pairs.into_iter().collect();
        let canonical = canonical_group_tags(&tags);
        let digest = stable_hash_hex(&canonical);
        let id = format!("group-{}", &digest[..16]);
        let palette_index = usize::from_str_radix(&digest[..8], 16).unwrap_or(0) % PALETTE.len();
        let color = PALETTE[palette_index].to_string();
        let label = group_label(&tags);
        let mut investigations = Vec::with_capacity(members.len());
        let mut included = Vec::new();
        let mut excluded = Vec::new();
        // Store complete rows before averaging. Besides enforcing one shared
        // cohort, this avoids an overflowing raw sum for legal f64 grades.
        let mut complete_rows: Vec<Vec<f64>> = Vec::new();

        for grouped in members {
            let snap = &grouped.snapshot;
            investigations.push(snap.id.clone());
            let missing_grades: Vec<String> = req
                .axes
                .iter()
                .filter(|a| {
                    reserved_direction(&a.name).is_none() && !snap.grades.contains_key(&a.name)
                })
                .map(|a| a.name.clone())
                .collect();
            let missing_axes: Vec<String> = req
                .axes
                .iter()
                .filter(|a| {
                    if reserved_direction(&a.name).is_some() {
                        !matches!(resolve_reserved(snap, &a.name), Some(value) if value.is_finite())
                    } else {
                        // PATCH validation rejects these, but core is public:
                        // direct callers get transparent unavailable evidence,
                        // never NaN/Infinity arithmetic or a renderer panic.
                        matches!(snap.grades.get(&a.name), Some(value) if !value.is_finite())
                    }
                })
                .map(|a| a.name.clone())
                .collect();
            let status = match snap.status {
                SnapshotStatus::Running => Some("running"),
                SnapshotStatus::Failed => Some("failed"),
                SnapshotStatus::Done if !missing_grades.is_empty() => Some("awaiting_grades"),
                SnapshotStatus::Done if !missing_axes.is_empty() => Some("unavailable"),
                SnapshotStatus::Done => None,
            };
            if let Some(status) = status {
                excluded.push(GroupExclusion {
                    investigation: snap.id.clone(),
                    status: status.into(),
                    missing_grades,
                    missing_axes,
                });
                continue;
            }
            // Status and every requested value were checked above.
            complete_rows.push(
                req.axes
                    .iter()
                    .map(|axis| {
                        if reserved_direction(&axis.name).is_some() {
                            resolve_reserved(snap, &axis.name).expect("checked measured axis")
                        } else {
                            *snap.grades.get(&axis.name).expect("checked grade")
                        }
                    })
                    .collect(),
            );
            included.push(snap.id.clone());
        }
        investigations.sort();
        included.sort();
        excluded.sort_by(|a, b| a.investigation.cmp(&b.investigation));
        let values = (!included.is_empty()).then(|| {
            req.axes
                .iter()
                .enumerate()
                .map(|(i, axis)| (axis.name.clone(), finite_mean(&complete_rows, i)))
                .collect()
        });
        points.push(GroupedFrontierPoint {
            id,
            tags,
            label,
            color,
            investigations,
            included,
            preliminary: !excluded.is_empty(),
            excluded,
            values,
            on_frontier: None,
            dominated_by: Vec::new(),
        });
    }

    let dirs: Vec<f64> = req
        .axes
        .iter()
        .map(|a| match a.better {
            BetterDirection::Higher => 1.0,
            BetterDirection::Lower => -1.0,
        })
        .collect();
    for i in 0..points.len() {
        let Some(values) = points[i].values.as_ref() else {
            continue;
        };
        let mut dominated_by = Vec::new();
        for j in 0..points.len() {
            if i == j {
                continue;
            }
            let Some(other) = points[j].values.as_ref() else {
                continue;
            };
            let ge = req
                .axes
                .iter()
                .zip(&dirs)
                .all(|(axis, dir)| other[&axis.name] * dir >= values[&axis.name] * dir);
            let gt = req
                .axes
                .iter()
                .zip(&dirs)
                .any(|(axis, dir)| other[&axis.name] * dir > values[&axis.name] * dir);
            if ge && gt {
                dominated_by.push(points[j].id.clone());
            }
        }
        points[i].on_frontier = Some(dominated_by.is_empty());
        points[i].dominated_by = dominated_by;
    }
    Ok(GroupedFrontierResponse { points })
}

/// Overflow-safe mean of a column of known-finite values. Normalize by the
/// largest magnitude first, then use compensated summation; unlike `sum / n`,
/// this remains finite for e.g. two 1e308 grades.
fn finite_mean(rows: &[Vec<f64>], column: usize) -> f64 {
    let scale = rows.iter().map(|row| row[column].abs()).fold(0.0, f64::max);
    if scale == 0.0 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut compensation = 0.0;
    for row in rows {
        let value = row[column] / scale;
        let adjusted = value - compensation;
        let next = sum + adjusted;
        compensation = (next - sum) - adjusted;
        sum = next;
    }
    (sum / rows.len() as f64) * scale
}

fn group_label(tags: &BTreeMap<String, Option<String>>) -> String {
    if tags.is_empty() {
        return "all investigations".into();
    }
    tags.iter()
        .map(|(key, value)| match value {
            Some(value) => format!("{key}={value}"),
            None => format!("{key}=null"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::track::{UsageByRole, UsageTotals};

    fn snapshot(id: &str) -> GroupedSnapshot {
        GroupedSnapshot {
            snapshot: InvestigationSnapshot {
                id: id.into(),
                status: SnapshotStatus::Done,
                put_id: None,
                grades: BTreeMap::new(),
                usage: Some(UsageByRole::default()),
                put_model: None,
                sim_model: None,
                steps_per_trace: vec![1],
            },
            tags: BTreeMap::new(),
        }
    }
    fn request(axes: &[&str]) -> GroupedFrontierRequest {
        GroupedFrontierRequest {
            group_by: vec!["variant".into()],
            axes: axes
                .iter()
                .map(|name| FrontierAxis {
                    name: (*name).into(),
                    better: BetterDirection::Higher,
                })
                .collect(),
        }
    }
    fn compute_for(
        req: &GroupedFrontierRequest,
        values: Vec<GroupedSnapshot>,
    ) -> GroupedFrontierResponse {
        let snapshots = values
            .into_iter()
            .map(|s| (s.snapshot.id.clone(), s))
            .collect();
        compute_grouped(req, &snapshots, FrontierFormat::Json).unwrap()
    }

    #[test]
    fn means_use_one_shared_complete_cohort_and_keep_backlog() {
        let mut a1 = snapshot("a1");
        a1.tags.insert("variant".into(), "a".into());
        a1.snapshot
            .grades
            .extend([(String::from("x"), 2.), (String::from("y"), 4.)]);
        let mut a2 = snapshot("a2");
        a2.tags.insert("variant".into(), "a".into());
        a2.snapshot.grades.insert("x".into(), 100.); // must not leak into x mean
        let mut b = snapshot("b");
        b.tags.insert("variant".into(), "b".into());
        b.snapshot
            .grades
            .extend([(String::from("x"), 3.), (String::from("y"), 3.)]);
        let result = compute_for(&request(&["x", "y"]), vec![a1, a2, b]);
        let a = result
            .points
            .iter()
            .find(|p| p.tags["variant"] == Some("a".into()))
            .unwrap();
        assert_eq!(a.included, vec!["a1"]);
        assert_eq!(a.values.as_ref().unwrap()["x"], 2.0);
        assert_eq!(a.values.as_ref().unwrap()["y"], 4.0);
        assert_eq!(a.excluded[0].status, "awaiting_grades");
        assert_eq!(a.excluded[0].missing_grades, vec!["y"]);
    }

    #[test]
    fn running_failed_unpriced_and_no_data_are_visible_exclusions() {
        let mut running = snapshot("running");
        running.snapshot.status = SnapshotStatus::Running;
        let mut failed = snapshot("failed");
        failed.snapshot.status = SnapshotStatus::Failed;
        let mut cost = snapshot("cost");
        cost.snapshot.usage = Some(UsageByRole {
            put: UsageTotals::default(),
            sim: UsageTotals::default(),
        });
        cost.snapshot.grades.insert("grade".into(), 1.0);
        let req = GroupedFrontierRequest {
            group_by: vec![],
            axes: vec![
                FrontierAxis {
                    name: "grade".into(),
                    better: BetterDirection::Higher,
                },
                FrontierAxis {
                    name: "put_cost_usd".into(),
                    better: BetterDirection::Lower,
                },
            ],
        };
        let point = compute_for(&req, vec![running, failed, cost])
            .points
            .pop()
            .unwrap();
        assert!(point.values.is_none() && point.on_frontier.is_none() && point.preliminary);
        assert_eq!(
            point
                .excluded
                .iter()
                .map(|e| e.status.as_str())
                .collect::<Vec<_>>(),
            vec!["unavailable", "failed", "running"]
        );
        assert!(
            point
                .excluded
                .iter()
                .find(|e| e.investigation == "running")
                .unwrap()
                .missing_grades
                .contains(&"grade".into())
        );
    }

    #[test]
    fn completed_run_enters_frontier_and_tags_distinguish_null_from_string() {
        let mut missing = snapshot("missing");
        missing.snapshot.status = SnapshotStatus::Running;
        let mut literal = snapshot("literal");
        literal.tags.insert("variant".into(), "null".into());
        literal.snapshot.grades.insert("x".into(), 1.);
        let mut done = snapshot("done");
        done.snapshot.grades.insert("x".into(), 2.);
        let req = request(&["x"]);
        let before = compute_for(&req, vec![missing.clone(), literal.clone()]);
        assert_eq!(before.points.len(), 2); // absent tag bucket != literal "null"
        let mut after_missing = missing;
        after_missing.snapshot.status = SnapshotStatus::Done;
        after_missing.snapshot.grades.insert("x".into(), 3.);
        let after = compute_for(&req, vec![after_missing, literal, done]);
        assert!(
            after
                .points
                .iter()
                .any(|p| p.included.contains(&"done".into()) && p.on_frontier == Some(true))
        );
    }

    #[test]
    fn ids_and_colors_survive_membership_changes_and_empty_server_succeeds() {
        let mut one = snapshot("one");
        one.tags.insert("variant".into(), "same".into());
        one.snapshot.grades.insert("x".into(), 1.);
        let once = compute_for(&request(&["x"]), vec![one.clone()]);
        let mut two = snapshot("two");
        two.tags.insert("variant".into(), "same".into());
        two.snapshot.grades.insert("x".into(), 2.);
        let twice = compute_for(&request(&["x"]), vec![one, two]);
        assert_eq!(once.points[0].id, twice.points[0].id);
        assert_eq!(once.points[0].color, twice.points[0].color);
        let empty =
            compute_grouped(&request(&["x"]), &BTreeMap::new(), FrontierFormat::Json).unwrap();
        assert!(empty.points.is_empty());
    }

    #[test]
    fn canonical_group_identity_ignores_group_key_order() {
        let mut one = snapshot("one");
        one.tags.extend([
            (String::from("a"), String::from("one")),
            (String::from("b"), String::from("two")),
        ]);
        one.snapshot.grades.insert("x".into(), 1.0);
        let ordered = compute_for(
            &GroupedFrontierRequest {
                group_by: vec!["a".into(), "b".into()],
                axes: request(&["x"]).axes,
            },
            vec![one.clone()],
        );
        let reversed = compute_for(
            &GroupedFrontierRequest {
                group_by: vec!["b".into(), "a".into()],
                axes: request(&["x"]).axes,
            },
            vec![one],
        );
        assert_eq!(ordered.points[0].id, reversed.points[0].id);
        assert_eq!(ordered.points[0].color, reversed.points[0].color);
    }

    #[test]
    fn extreme_finite_grades_average_without_overflow_and_nonfinite_is_unavailable() {
        let mut huge_a = snapshot("huge-a");
        huge_a.snapshot.grades.insert("x".into(), 1e308);
        let mut huge_b = snapshot("huge-b");
        huge_b.snapshot.grades.insert("x".into(), 1e308);
        let point = compute_for(&request(&["x"]), vec![huge_a, huge_b])
            .points
            .pop()
            .unwrap();
        assert_eq!(point.values.as_ref().unwrap()["x"], 1e308);
        assert!(point.values.as_ref().unwrap()["x"].is_finite());

        let mut invalid = snapshot("invalid");
        invalid.snapshot.grades.insert("x".into(), f64::INFINITY);
        let point = compute_for(&request(&["x"]), vec![invalid])
            .points
            .pop()
            .unwrap();
        assert!(point.values.is_none());
        assert_eq!(point.excluded[0].status, "unavailable");
        assert_eq!(point.excluded[0].missing_axes, vec!["x"]);
    }

    #[test]
    fn malformed_request_rejects_unknown_duplicate_and_bad_group_tag() {
        let defaulted: GroupedFrontierRequest =
            serde_json::from_str(r#"{"axes":[{"name":"x","better":"higher"}]}"#).unwrap();
        assert_eq!(
            defaulted.group_by,
            vec!["put_model", "put_thinking", "prompt_hash"]
        );
        assert!(
            serde_json::from_str::<GroupedFrontierRequest>(r#"{"axes":[],"investigations":[]}"#)
                .is_err()
        );
        let req = GroupedFrontierRequest {
            group_by: vec!["Bad".into(), "Bad".into()],
            axes: vec![FrontierAxis {
                name: "x".into(),
                better: BetterDirection::Higher,
            }],
        };
        let error = compute_grouped(&req, &BTreeMap::new(), FrontierFormat::Json).unwrap_err();
        assert!(error.problems.iter().any(|p| p.reason == "bad_group_tag"));
        assert!(
            error
                .problems
                .iter()
                .any(|p| p.reason == "duplicate_group_tag")
        );
    }
}
