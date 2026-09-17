//! Hand-rolled SVG rendering of a 2-axis Pareto frontier plot — no
//! charting dependency (SVG is XML; a scatter + staircase frontier is
//! a few deterministic builders, and determinism makes it
//! golden-testable).
//!
//! Orientation contract: lower-is-better axes are pixel-inverted so
//! that **up-and-right is always better** — the frontier always reads
//! as an upper-right envelope, whichever directions the axes have.
//!
//! Defense in depth: every text is XML-escaped here even though labels
//! and colors are allow-pattern-validated before they ever reach the
//! renderer.

use std::collections::{BTreeMap, BTreeSet};

use super::grouped::GroupedFrontierResponse;
use super::{BetterDirection, FrontierPoint};

/// One rendered axis: name + direction. (The request-level
/// `FrontierAxis` is input-only; this is the resolved render spec.)
#[derive(Debug, Clone)]
pub struct PlotAxis {
    pub name: String,
    pub better: BetterDirection,
}

impl PlotAxis {
    pub fn new(name: &str, better: BetterDirection) -> Self {
        Self {
            name: name.to_string(),
            better,
        }
    }
}

// Layout constants (deterministic output = golden-testable output).
const W: f64 = 760.0;
const H: f64 = 520.0;
const L: f64 = 78.0; // left margin (y tick labels)
const R: f64 = 28.0; // right margin
const T: f64 = 36.0; // top margin (titles)
const B: f64 = 64.0; // bottom margin (x ticks + title)

// Inline plots inherit these semantic CSS variables from the UI, so switching
// theme updates existing SVGs immediately, without a refetch. Standalone SVGs
// have explicit Solarized Light fallbacks. Categorical point hues and the
// preference-neutral violet frontier stay identical across themes.
const TICK_COLOR: &str = "var(--frontier-tick, #657b83)";
const LABEL_COLOR: &str = "var(--frontier-label, #586e75)";
const DIM_LABEL_COLOR: &str = "var(--frontier-dim-label, #93a1a1)";
const GRID_COLOR: &str = "var(--frontier-grid, #93a1a1)";
const PANEL_BG: &str = "var(--frontier-panel, #fdf6e3)";
const PAGE_BG: &str = "var(--frontier-page, #eee8d5)";
const FRONTIER_STROKE: &str = "#6c71c4";

/// XML-escape a text run and discard XML-forbidden control code points. Attributes
/// are caller-owned arbitrary text, so escaping alone is not enough to make a
/// valid SVG document. Valid Unicode (including non-ASCII labels) is retained.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if !matches!(c, '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}') =>
                {}
            _ => out.push(c),
        }
    }
    out
}

/// Tick-number formatting: rounded to the precision the STEP implies
/// (nice steps are multiples of 1/2/2.5/5×10^k, so this kills the
/// `0.6000000000000001` accumulation noise), trailing zeros trimmed;
/// exponent notation for extreme magnitudes.
fn fmt_tick(v: f64, step: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-6..1e16).contains(&a) {
        return format!("{v:e}");
    }
    let decimals = ((-(step.abs().log10()).floor()) as i32 + 1).clamp(0, 15) as usize;
    let s = format!("{v:.decimals$}");
    // Trim trailing zeros only when a decimal point exists (otherwise
    // "2500" would become "25").
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Nice-numbers tick selection over a data range: returns (ticks,
/// plot_lo, plot_hi) where the plot range is padded to whole steps.
/// A degenerate (equal lo/hi) range is padded symmetrically.
fn nice_ticks(lo: f64, hi: f64, target: usize) -> (Vec<f64>, f64, f64) {
    // Opposite-sign values near f64::MAX make `hi - lo` overflow even
    // though every plotted coordinate is finite. Normalize before choosing
    // nice ticks, then scale back; ordinary ranges retain byte-for-byte legacy
    // behavior (and its SVG golden).
    if !lo.is_finite() || !hi.is_finite() {
        return (vec![-1.0, 0.0, 1.0], -1.0, 1.0);
    }
    if !(hi - lo).is_finite() {
        let scale = lo.abs().max(hi.abs());
        if scale > 0.0 {
            let (ticks, plo, phi) = nice_ticks(lo / scale, hi / scale, target);
            return (
                ticks.into_iter().map(|v| v * scale).collect(),
                plo * scale,
                phi * scale,
            );
        }
    }
    let (lo, hi) = if lo == hi {
        let h = if lo == 0.0 {
            0.5
        } else {
            (lo.abs() * 0.25).max(1e-9)
        };
        (lo - h, lo + h)
    } else {
        (lo, hi)
    };
    let range = hi - lo;
    let step0 = range / target.max(1) as f64;
    let mag = 10f64.powf(step0.log10().floor());
    let norm = step0 / mag;
    let step = (if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    }) * mag;
    let plo = (lo / step).floor() * step;
    let phi = (hi / step).ceil() * step;
    let n = ((phi - plo) / step).round() as i64;
    let mut ticks = Vec::new();
    for k in 0..=n.min(20) {
        ticks.push(plo + k as f64 * step);
    }
    (ticks, plo, phi)
}

/// Map a value to a pixel coordinate along one axis. `a`/`b` are the
/// pixel coords spanning the plot (a = left/bottom, b = right/top).
/// Lower-is-better axes invert, so the BETTER end always lands at `b`
/// (right for x, top for y) — the up-and-right-is-better contract.
fn px(v: f64, plo: f64, phi: f64, a: f64, b: f64, better: BetterDirection) -> f64 {
    let range = phi - plo;
    let t = if range.is_finite() {
        (v - plo) / range
    } else {
        let scale = v.abs().max(plo.abs()).max(phi.abs());
        if scale == 0.0 {
            0.5
        } else {
            (v / scale - plo / scale) / (phi / scale - plo / scale)
        }
    }
    .clamp(0.0, 1.0);
    match better {
        BetterDirection::Higher => a + t * (b - a),
        BetterDirection::Lower => b - t * (b - a),
    }
}

/// Render the frontier scatter plot. Exactly 2 axes (the arity is
/// enforced upstream — `compute` rejects `format=svg` with ≠ 2 axes).
pub fn render(points: &[FrontierPoint], x: &PlotAxis, y: &PlotAxis) -> String {
    let xs: Vec<f64> = points.iter().map(|p| p.values[&x.name]).collect();
    let ys: Vec<f64> = points.iter().map(|p| p.values[&y.name]).collect();
    let (xt, xlo, xhi) = nice_ticks(
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        5,
    );
    let (yt, ylo, yhi) = nice_ticks(
        ys.iter().cloned().fold(f64::INFINITY, f64::min),
        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        5,
    );
    let step_x = xt.get(1).copied().unwrap_or(xhi) - xt.first().copied().unwrap_or(xhi);
    let step_y = yt.get(1).copied().unwrap_or(yhi) - yt.first().copied().unwrap_or(yhi);

    let plot_w = W - L - R;
    let plot_h = H - T - B;

    let mut s = String::with_capacity(4096);
    s.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="ui-monospace, Menlo, Consolas, monospace">"#
    ));
    s.push_str(&format!(
        r#"<rect width="{W}" height="{H}" fill="{PAGE_BG}"/>"#
    ));
    // Title + orientation note.
    s.push_str(&format!(
        r#"<text x="{L}" y="21" font-size="13" font-weight="700" fill="{LABEL_COLOR}">Pareto frontier</text>"#
    ));
    s.push_str(&format!(
        r#"<text x="{}" y="21" font-size="11" fill="{TICK_COLOR}" text-anchor="end">up &amp; right is better</text>"#,
        W - R
    ));

    // Plot panel.
    s.push_str(&format!(
        r#"<rect x="{L}" y="{T}" width="{plot_w}" height="{plot_h}" fill="{PANEL_BG}" stroke="{GRID_COLOR}"/>"#
    ));

    // Gridlines + ticks.
    for &t in &xt {
        let tx = px(t, xlo, xhi, L, W - R, x.better);
        s.push_str(&format!(
            r#"<line x1="{tx:.1}" y1="{T}" x2="{tx:.1}" y2="{}" stroke="{GRID_COLOR}"/>"#,
            H - B
        ));
        s.push_str(&format!(
            r#"<text x="{tx:.1}" y="{}" font-size="11" fill="{TICK_COLOR}" text-anchor="middle">{}</text>"#,
            H - B + 18.0,
            esc(&fmt_tick(t, step_x))
        ));
    }
    for &t in &yt {
        let ty = px(t, ylo, yhi, H - B, T, y.better);
        s.push_str(&format!(
            r#"<line x1="{L}" y1="{ty:.1}" x2="{}" y2="{ty:.1}" stroke="{GRID_COLOR}"/>"#,
            W - R
        ));
        s.push_str(&format!(
            r#"<text x="{}" y="{:.1}" font-size="11" fill="{TICK_COLOR}" text-anchor="end">{}</text>"#,
            L - 8.0,
            ty + 4.0,
            esc(&fmt_tick(t, step_y))
        ));
    }

    // Axis titles carry the direction AND that right/up is better.
    s.push_str(&format!(
        r#"<text x="{:.1}" y="{}" font-size="12" fill="{TICK_COLOR}" text-anchor="middle">{} &#8212; {} is better &#8594;</text>"#,
        L + plot_w / 2.0,
        H - 14.0,
        esc(&x.name),
        x.better.as_str()
    ));
    // Y-axis title runs rotated up the left margin (clear of the tick
    // labels, which end at L-8, and of the main title at top-left).
    let y_mid = (T + (H - B)) / 2.0;
    s.push_str(&format!(
        r#"<text transform="translate(16 {y_mid:.1}) rotate(-90)" font-size="12" fill="{TICK_COLOR}" text-anchor="middle">{} &#8212; {} is better &#8593;</text>"#,
        esc(&y.name),
        y.better.as_str()
    ));

    // Frontier staircase: through the non-dominated set, pixel-sorted
    // by x ascending. In up-right-better orientation the frontier
    // descends left→right; the path starts at the left edge at the
    // first point's height and exits at the right edge at the last's.
    let mut frontier: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.on_frontier)
        .map(|p| {
            (
                px(p.values[&x.name], xlo, xhi, L, W - R, x.better),
                px(p.values[&y.name], ylo, yhi, H - B, T, y.better),
            )
        })
        .collect();
    frontier.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then(a.1.partial_cmp(&b.1).unwrap())
    });
    if let Some((x0, y0)) = frontier.first() {
        let mut d = format!("M {L:.1} {y0:.1} L {x0:.1} {y0:.1}");
        let mut prev_y = *y0;
        for &(cx, cy) in frontier.iter().skip(1) {
            d.push_str(&format!(" L {cx:.1} {prev_y:.1} L {cx:.1} {cy:.1}"));
            prev_y = cy;
        }
        d.push_str(&format!(" L {:.1} {prev_y:.1}", W - R));
        s.push_str(&format!(
            r#"<path d="{d}" fill="none" stroke="{FRONTIER_STROKE}" stroke-width="2" opacity="0.85"/>"#
        ));
    }

    // Points + labels.
    for p in points {
        let cx = px(p.values[&x.name], xlo, xhi, L, W - R, x.better);
        let cy = px(p.values[&y.name], ylo, yhi, H - B, T, y.better);
        if p.on_frontier {
            s.push_str(&format!(
                r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="6" fill="{}" stroke="{PANEL_BG}" stroke-width="1.5"/>"#,
                esc(&p.color)
            ));
        } else {
            // Dominated: same colour, drawn as a hollow ring — visible
            // identity without competing with the filled frontier dots.
            s.push_str(&format!(
                r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="4.5" fill="none" stroke="{}" stroke-width="1.8"/>"#,
                esc(&p.color)
            ));
        }
        // Flip the label to the left when it would overflow the right
        // edge (rough width estimate: ~6.8px per char at 11px mono).
        let w_est = p.label.len() as f64 * 6.8;
        let (lx, anchor) = if cx + 9.0 + w_est > W - 6.0 {
            (cx - 9.0, "end")
        } else {
            (cx + 9.0, "start")
        };
        let fill = if p.on_frontier {
            LABEL_COLOR
        } else {
            DIM_LABEL_COLOR
        };
        // Near the top edge an above-point label would collide with the
        // corner hint / title row, so drop it below the point instead.
        let ly = if cy - 20.0 < T + 8.0 {
            cy + 20.0
        } else {
            cy - 9.0
        };
        s.push_str(&format!(
            r#"<text x="{lx:.1}" y="{ly:.1}" font-size="11" fill="{fill}" text-anchor="{anchor}">{}</text>"#,
            esc(&p.label)
        ));
    }

    s.push_str("</svg>");
    s
}

fn wrap_pending(text: &str) -> Vec<String> {
    const WIDTH: usize = 88;
    // Wrap raw text first: splitting an escaped '&amp;' between text nodes
    // would produce invalid XML. Escape each completed line at rendering.
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if !line.is_empty() && line.chars().count() + 1 + word_len > WIDTH {
            lines.push(line);
            line = String::new();
        }
        // Long hashes/ids have no whitespace. Split them by Unicode scalar so
        // no text element can exceed the bounded plot width.
        if word_len > WIDTH {
            if !line.is_empty() {
                lines.push(line);
                line = String::new();
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(WIDTH) {
                lines.push(chunk.iter().collect());
            }
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn pending_description(point: &super::grouped::GroupedFrontierPoint) -> String {
    let attributes = point
        .attributes
        .iter()
        .map(|(key, value)| match value {
            Some(value) => format!("{key}={value}"),
            None => format!("{key}=null"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let excluded = point
        .excluded
        .iter()
        .map(|e| {
            let mut detail = format!("{} ({})", e.investigation, e.status);
            if !e.missing_grades.is_empty() {
                detail.push_str(&format!(" grades={}", e.missing_grades.join(",")));
            }
            if !e.missing_axes.is_empty() {
                detail.push_str(&format!(" axes={}", e.missing_axes.join(",")));
            }
            detail
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{}: attributes [{}]; members={}; included={}; excluded [{}]",
        point.label,
        attributes,
        point.investigations.len(),
        point.included.len(),
        excluded
    )
}

/// Render grouped frontier points. Complete groups use their real arithmetic
/// means; groups with no common cohort are listed as a wrapped backlog below
/// the plot rather than receiving invented coordinates. Preliminary points keep
/// their filled/hollow dominance shape but the entire marker group is subdued.
const LEGEND_GAP: f64 = 22.0;
const MIN_LEGEND_COLUMN_WIDTH: f64 = 220.0;
const LEGEND_ROW_HEIGHT: f64 = 24.0;

/// Reserve enough horizontal space for the complete longest label. SVG uses a
/// monospace stack, but non-ASCII fallback glyphs can be roughly double-width;
/// counting them as two cells keeps columns separate without clipping text.
fn legend_column_width(points: &[super::grouped::GroupedFrontierPoint]) -> f64 {
    let max_cells = points
        .iter()
        .map(|point| {
            point
                .label
                .chars()
                .map(|c| if c.is_ascii() { 1 } else { 2 })
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    (max_cells as f64 * 6.8 + 32.0).max(MIN_LEGEND_COLUMN_WIDTH)
}

/// Order the legend along the first principal component of normalized SCREEN
/// coordinates. This is orthogonal regression, so vertical clouds are handled
/// as naturally as horizontal ones. The sign is deterministic: left-to-right,
/// or top-to-bottom when effectively vertical. Isotropic/degenerate clouds use
/// the stable x-then-y fallback. Pending groups follow plotted groups by label.
fn ordered_legend<'a>(
    usable: &[(&'a super::grouped::GroupedFrontierPoint, f64, f64)],
    unplotted: &[&'a super::grouped::GroupedFrontierPoint],
    xlo: f64,
    xhi: f64,
    ylo: f64,
    yhi: f64,
    x: &PlotAxis,
    y: &PlotAxis,
) -> (
    Vec<&'a super::grouped::GroupedFrontierPoint>,
    Option<(f64, f64, f64, f64)>, // mean x/y + principal direction in common-scale screen coordinates
) {
    let plot_w = W - L - R;
    let plot_h = H - T - B;
    // Preserve the aspect ratio the reader actually sees. Dividing both pixel
    // axes by ONE common scale keeps magnitudes bounded without warping the
    // plot into a unit square (which can swap near-adjacent projections).
    let common_scale = plot_w.max(plot_h);
    let mut positioned: Vec<_> = usable
        .iter()
        .map(|(point, xv, yv)| {
            let sx = (px(*xv, xlo, xhi, L, W - R, x.better) - L) / common_scale;
            let sy = (px(*yv, ylo, yhi, H - B, T, y.better) - T) / common_scale;
            (*point, sx, sy, 0.0)
        })
        .collect();

    let mut principal_axis = None;
    if positioned.len() >= 2 {
        let n = positioned.len() as f64;
        let mx = positioned.iter().map(|(_, sx, _, _)| sx).sum::<f64>() / n;
        let my = positioned.iter().map(|(_, _, sy, _)| sy).sum::<f64>() / n;
        let sxx = positioned
            .iter()
            .map(|(_, sx, _, _)| (sx - mx).powi(2))
            .sum::<f64>();
        let syy = positioned
            .iter()
            .map(|(_, _, sy, _)| (sy - my).powi(2))
            .sum::<f64>();
        let sxy = positioned
            .iter()
            .map(|(_, sx, sy, _)| (sx - mx) * (sy - my))
            .sum::<f64>();
        let total = sxx + syy;
        let anisotropy = ((sxx - syy).powi(2) + 4.0 * sxy.powi(2)).sqrt();
        if total > 0.0 && anisotropy > total * 1e-9 {
            let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
            let (mut vx, mut vy) = (angle.cos(), angle.sin());
            if vx < -1e-12 || (vx.abs() <= 1e-12 && vy < 0.0) {
                vx = -vx;
                vy = -vy;
            }
            principal_axis = Some((mx, my, vx, vy));
            for (_, sx, sy, projection) in &mut positioned {
                *projection = (*sx - mx) * vx + (*sy - my) * vy;
            }
            positioned.sort_by(|a, b| {
                a.3.total_cmp(&b.3)
                    .then(a.1.total_cmp(&b.1))
                    .then(a.2.total_cmp(&b.2))
                    .then(a.0.id.cmp(&b.0.id))
            });
        } else {
            positioned.sort_by(|a, b| {
                a.1.total_cmp(&b.1)
                    .then(a.2.total_cmp(&b.2))
                    .then(a.0.id.cmp(&b.0.id))
            });
        }
    }

    let mut ordered: Vec<_> = positioned
        .into_iter()
        .map(|(point, _, _, _)| point)
        .collect();
    let mut pending = unplotted.to_vec();
    pending.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    ordered.extend(pending);
    (ordered, principal_axis)
}

/// Render grouped frontier points with a PCA-ordered legend to the right.
/// Point-adjacent labels are intentionally absent: the legend is the single
/// uncluttered label surface. Each marker and its legend entry share one SVG
/// group, making hover and keyboard focus bidirectional even in standalone SVG.
pub fn render_grouped(response: &GroupedFrontierResponse, x: &PlotAxis, y: &PlotAxis) -> String {
    let usable: Vec<_> = response
        .points
        .iter()
        .filter_map(|p| {
            let values = p.values.as_ref()?;
            let xv = *values.get(&x.name)?;
            let yv = *values.get(&y.name)?;
            (xv.is_finite() && yv.is_finite()).then_some((p, xv, yv))
        })
        .collect();
    let usable_ids: BTreeSet<&str> = usable
        .iter()
        .map(|(point, _, _)| point.id.as_str())
        .collect();
    // Public core callers can construct malformed/non-finite value maps even
    // though production grouping cannot. Keep those groups visible as pending
    // instead of silently omitting their legend/backlog evidence.
    let unplotted: Vec<_> = response
        .points
        .iter()
        .filter(|p| !usable_ids.contains(p.id.as_str()))
        .collect();
    let backlog: Vec<_> = response
        .points
        .iter()
        .filter(|p| !usable_ids.contains(p.id.as_str()) || !p.excluded.is_empty())
        .collect();
    let pending_lines: Vec<String> = backlog
        .iter()
        .flat_map(|p| wrap_pending(&pending_description(p)))
        .collect();
    let svg_h = H + if pending_lines.is_empty() {
        0.0
    } else {
        22.0 + pending_lines.len() as f64 * 13.0
    };
    let legend_rows = ((H - T - B) / LEGEND_ROW_HEIGHT).floor().max(1.0) as usize;
    let legend_columns = if response.points.is_empty() {
        0
    } else {
        response.points.len().div_ceil(legend_rows)
    };
    let legend_column_width = legend_column_width(&response.points);
    let svg_w = W + if legend_columns == 0 {
        0.0
    } else {
        LEGEND_GAP + legend_columns as f64 * legend_column_width
    };
    let plot_w = W - L - R;
    let plot_h = H - T - B;
    let mut s = String::with_capacity(8192 + pending_lines.len() * 100);
    s.push_str(&format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{svg_w}" height="{svg_h}" viewBox="0 0 {svg_w} {svg_h}" font-family="ui-monospace, Menlo, Consolas, monospace">"#));
    s.push_str(&format!(
        r#"<style>
.frontier-group {{ cursor:pointer; outline:none; transition:opacity .12s ease; }}
.frontier-group.preliminary {{ opacity:.48; }}
.frontier-group:hover, .frontier-group:focus {{ opacity:1; }}
svg:has(.frontier-group:hover) .frontier-group:not(:hover),
svg:has(.frontier-group:focus) .frontier-group:not(:focus) {{ opacity:.18; }}
.focus-halo {{ opacity:0; }}
.frontier-group:hover .focus-halo, .frontier-group:focus .focus-halo {{ opacity:1; }}
.legend-label {{ fill:{LABEL_COLOR}; font-size:11px; }}
.frontier-group.pending .legend-label {{ fill:{DIM_LABEL_COLOR}; }}
</style>"#
    ));
    s.push_str(&format!(r#"<defs><clipPath id="pca-plot-clip"><rect x="{L}" y="{T}" width="{plot_w}" height="{plot_h}"/></clipPath></defs>"#));
    s.push_str(&format!(
        r#"<rect width="{svg_w}" height="{svg_h}" fill="{PAGE_BG}"/>"#
    ));
    s.push_str(&format!(r#"<text x="{L}" y="21" font-size="13" font-weight="700" fill="{LABEL_COLOR}">Grouped Pareto frontier</text>"#));
    s.push_str(&format!(r#"<text x="{}" y="21" font-size="11" fill="{TICK_COLOR}" text-anchor="end">up &amp; right is better</text>"#, W - R));
    if legend_columns > 0 {
        s.push_str(&format!(r#"<text x="{}" y="21" font-size="11" font-weight="700" fill="{LABEL_COLOR}">groups</text>"#, W + LEGEND_GAP));
    }
    s.push_str(&format!(r#"<rect x="{L}" y="{T}" width="{plot_w}" height="{plot_h}" fill="{PANEL_BG}" stroke="{GRID_COLOR}"/>"#));

    let (xt, xlo, xhi, yt, ylo, yhi) = if usable.is_empty() {
        s.push_str(&format!(r#"<text x="{:.1}" y="{:.1}" font-size="13" fill="{DIM_LABEL_COLOR}" text-anchor="middle">no groups have a complete cohort for these axes</text>"#, L + plot_w / 2.0, T + plot_h / 2.0));
        (vec![0.0, 1.0], 0.0, 1.0, vec![0.0, 1.0], 0.0, 1.0)
    } else {
        let (xt, xlo, xhi) = nice_ticks(
            usable
                .iter()
                .map(|(_, v, _)| *v)
                .fold(f64::INFINITY, f64::min),
            usable
                .iter()
                .map(|(_, v, _)| *v)
                .fold(f64::NEG_INFINITY, f64::max),
            5,
        );
        let (yt, ylo, yhi) = nice_ticks(
            usable
                .iter()
                .map(|(_, _, v)| *v)
                .fold(f64::INFINITY, f64::min),
            usable
                .iter()
                .map(|(_, _, v)| *v)
                .fold(f64::NEG_INFINITY, f64::max),
            5,
        );
        (xt, xlo, xhi, yt, ylo, yhi)
    };

    let (legend, principal_axis) = ordered_legend(&usable, &unplotted, xlo, xhi, ylo, yhi, x, y);

    if !usable.is_empty() {
        let step_x = xt.get(1).copied().unwrap_or(xhi) - xt.first().copied().unwrap_or(xhi);
        let step_y = yt.get(1).copied().unwrap_or(yhi) - yt.first().copied().unwrap_or(yhi);
        for &tick in &xt {
            let at = px(tick, xlo, xhi, L, W - R, x.better);
            s.push_str(&format!(
                r#"<line x1="{at:.1}" y1="{T}" x2="{at:.1}" y2="{}" stroke="{GRID_COLOR}"/>"#,
                H - B
            ));
            s.push_str(&format!(r#"<text x="{at:.1}" y="{}" font-size="11" fill="{TICK_COLOR}" text-anchor="middle">{}</text>"#, H - B + 18.0, esc(&fmt_tick(tick, step_x))));
        }
        for &tick in &yt {
            let at = px(tick, ylo, yhi, H - B, T, y.better);
            s.push_str(&format!(
                r#"<line x1="{L}" y1="{at:.1}" x2="{}" y2="{at:.1}" stroke="{GRID_COLOR}"/>"#,
                W - R
            ));
            s.push_str(&format!(r#"<text x="{}" y="{:.1}" font-size="11" fill="{TICK_COLOR}" text-anchor="end">{}</text>"#, L - 8.0, at + 4.0, esc(&fmt_tick(tick, step_y))));
        }
        // Temporary visual aid while validating legend order. It uses the
        // exact PCA mean/direction that drives projection sorting, clipped to
        // the plot rectangle so the right-side legend remains untouched.
        if let Some((mx, my, vx, vy)) = principal_axis {
            let common_scale = plot_w.max(plot_h);
            let cx = L + mx * common_scale;
            let cy = T + my * common_scale;
            let reach = 2000.0;
            s.push_str(&format!(r##"<g class="pca-helper" opacity="0.72" pointer-events="none" clip-path="url(#pca-plot-clip)"><line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="#b58900" stroke-width="1.5" stroke-dasharray="7 5"/><text x="{:.1}" y="{:.1}" font-size="10" fill="#b58900">legend PCA</text></g>"##, cx - vx * reach, cy - vy * reach, cx + vx * reach, cy + vy * reach, cx + 7.0, cy - 7.0));
        }

        let mut frontier: Vec<(f64, f64)> = usable
            .iter()
            .filter(|(p, _, _)| p.on_frontier == Some(true))
            .map(|(_, xv, yv)| {
                (
                    px(*xv, xlo, xhi, L, W - R, x.better),
                    px(*yv, ylo, yhi, H - B, T, y.better),
                )
            })
            .collect();
        frontier.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        if let Some((x0, y0)) = frontier.first() {
            let mut d = format!("M {L:.1} {y0:.1} L {x0:.1} {y0:.1}");
            let mut prev_y = *y0;
            for &(cx, cy) in frontier.iter().skip(1) {
                d.push_str(&format!(" L {cx:.1} {prev_y:.1} L {cx:.1} {cy:.1}"));
                prev_y = cy;
            }
            d.push_str(&format!(" L {:.1} {prev_y:.1}", W - R));
            s.push_str(&format!(r#"<path d="{d}" fill="none" stroke="{FRONTIER_STROKE}" stroke-width="2" opacity="0.85"/>"#));
        }
    }

    let coordinate_by_id: BTreeMap<&str, (f64, f64)> = usable
        .iter()
        .map(|(point, xv, yv)| {
            (
                point.id.as_str(),
                (
                    px(*xv, xlo, xhi, L, W - R, x.better),
                    px(*yv, ylo, yhi, H - B, T, y.better),
                ),
            )
        })
        .collect();
    for (index, point) in legend.iter().enumerate() {
        let row = index % legend_rows;
        let column = index / legend_rows;
        let lx = W + LEGEND_GAP + column as f64 * legend_column_width + 7.0;
        let ly = T + row as f64 * LEGEND_ROW_HEIGHT + 11.0;
        let coordinates = coordinate_by_id.get(point.id.as_str()).copied();
        let frontier = point.on_frontier == Some(true);
        let pending = coordinates.is_none();
        let mut classes = String::from("frontier-group");
        if point.preliminary {
            classes.push_str(" preliminary");
        }
        if pending {
            classes.push_str(" pending");
        }
        let plot_state = match point.on_frontier {
            Some(true) => "frontier",
            Some(false) => "dominated",
            None => "pending",
        };
        let evidence_state = if point.preliminary {
            "preliminary"
        } else {
            "final"
        };
        let coordinate_text = coordinates
            .map(|_| {
                let values = point.values.as_ref().expect("plotted point has values");
                format!(
                    "; {}={}; {}={}",
                    x.name, values[&x.name], y.name, values[&y.name]
                )
            })
            .unwrap_or_default();
        let title = format!(
            "{}; {plot_state}; {evidence_state}{coordinate_text}; attributes: {}; members: {}; included: {}",
            point.label,
            point
                .attributes
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("null")))
                .collect::<Vec<_>>()
                .join(", "),
            point.investigations.len(),
            point.included.len()
        );
        s.push_str(&format!(r#"<g class="{classes}" data-group-id="{}" data-legend-column="{column}" tabindex="0" role="group" aria-label="{}"><title>{}</title>"#, esc(&point.id), esc(&title), esc(&title)));
        if let Some((cx, cy)) = coordinates {
            s.push_str(&format!(r#"<g class="plot-marker"><circle class="hit-target" cx="{cx:.1}" cy="{cy:.1}" r="11" fill="transparent" stroke="none" pointer-events="all"/><circle class="focus-halo" cx="{cx:.1}" cy="{cy:.1}" r="11" fill="none" stroke="{}" stroke-width="2.5"/>"#, esc(&point.color)));
            if frontier {
                s.push_str(&format!(r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="6" fill="{}" stroke="{PANEL_BG}" stroke-width="1.5"/>"#, esc(&point.color)));
            } else {
                s.push_str(&format!(r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="4.5" fill="none" stroke="{}" stroke-width="1.8"/>"#, esc(&point.color)));
            }
            if point.preliminary {
                s.push_str(&format!(r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="8.5" fill="none" stroke="{}" stroke-width="1.4" stroke-dasharray="3 2"/>"#, esc(&point.color)));
            }
            s.push_str("</g>");
        }
        s.push_str(&format!(r#"<g class="legend-entry" transform="translate({lx:.1} {ly:.1})"><circle class="focus-halo" cx="0" cy="0" r="9" fill="none" stroke="{}" stroke-width="2.5"/>"#, esc(&point.color)));
        if pending {
            s.push_str(&format!(r#"<circle cx="0" cy="0" r="5" fill="none" stroke="{DIM_LABEL_COLOR}" stroke-width="1.5" stroke-dasharray="2 2"/>"#));
        } else if frontier {
            s.push_str(&format!(
                r#"<circle cx="0" cy="0" r="5" fill="{}" stroke="{PANEL_BG}" stroke-width="1.2"/>"#,
                esc(&point.color)
            ));
        } else {
            s.push_str(&format!(
                r#"<circle cx="0" cy="0" r="4" fill="none" stroke="{}" stroke-width="1.6"/>"#,
                esc(&point.color)
            ));
        }
        if point.preliminary && !pending {
            s.push_str(&format!(r#"<circle cx="0" cy="0" r="7.5" fill="none" stroke="{}" stroke-width="1.2" stroke-dasharray="3 2"/>"#, esc(&point.color)));
        }
        s.push_str(&format!(
            r#"<text class="legend-label" x="13" y="4">{}</text></g></g>"#,
            esc(&point.label)
        ));
    }

    s.push_str(&format!(r#"<text x="{:.1}" y="{}" font-size="12" fill="{TICK_COLOR}" text-anchor="middle">{} &#8212; {} is better &#8594;</text>"#, L + plot_w / 2.0, H - 14.0, esc(&x.name), x.better.as_str()));
    let y_mid = (T + (H - B)) / 2.0;
    s.push_str(&format!(r#"<text transform="translate(16 {y_mid:.1}) rotate(-90)" font-size="12" fill="{TICK_COLOR}" text-anchor="middle">{} &#8212; {} is better &#8593;</text>"#, esc(&y.name), y.better.as_str()));
    if !pending_lines.is_empty() {
        s.push_str(&format!(r#"<text x="{L}" y="{}" font-size="11" fill="{DIM_LABEL_COLOR}">pending / preliminary backlog (excluded investigations):</text>"#, H + 16.0));
        for (i, line) in pending_lines.iter().enumerate() {
            s.push_str(&format!(
                r#"<text x="{L}" y="{}" font-size="10" fill="{DIM_LABEL_COLOR}">{}</text>"#,
                H + 30.0 + i as f64 * 13.0,
                esc(line)
            ));
        }
    }
    s.push_str("</svg>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_basic_range() {
        let (ticks, lo, hi) = nice_ticks(0.0, 10.0, 5);
        assert_eq!(ticks, vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]);
        assert_eq!((lo, hi), (0.0, 10.0));
    }

    #[test]
    fn nice_ticks_expands_to_whole_steps() {
        let (ticks, lo, hi) = nice_ticks(0.3, 9.7, 5);
        assert_eq!(ticks.first(), Some(&0.0));
        assert_eq!(ticks.last(), Some(&10.0));
        assert!(lo < 0.3 && hi > 9.7);
    }

    #[test]
    fn nice_ticks_degenerate_range_pads() {
        let (ticks, lo, hi) = nice_ticks(4.0, 4.0, 5);
        assert!(lo < 4.0 && hi > 4.0);
        assert!(!ticks.is_empty());
    }

    #[test]
    fn px_maps_better_end_right_for_lower_better() {
        // v = plo (the LOWEST value = best) must land at the RIGHT end.
        assert_eq!(
            px(0.0, 0.0, 10.0, 100.0, 200.0, BetterDirection::Lower),
            200.0
        );
        assert_eq!(
            px(10.0, 0.0, 10.0, 100.0, 200.0, BetterDirection::Lower),
            100.0
        );
        assert_eq!(
            px(10.0, 0.0, 10.0, 100.0, 200.0, BetterDirection::Higher),
            200.0
        );
    }

    #[test]
    fn esc_drops_xml_forbidden_noncharacters_but_preserves_unicode() {
        assert_eq!(esc("Ä\u{fffe}\u{ffff}\u{0}🍊"), "Ä🍊");
    }

    #[test]
    fn esc_escapes_xml_specials() {
        assert_eq!(esc(r#"<a&"b'>"#), "&lt;a&amp;&quot;b&#39;&gt;");
    }

    #[test]
    fn fmt_tick_kills_accumulation_noise() {
        assert_eq!(fmt_tick(0.6000000000000001, 0.025), "0.6");
        assert_eq!(fmt_tick(0.4, 0.025), "0.4");
        assert_eq!(fmt_tick(2500.0, 500.0), "2500");
        assert_eq!(fmt_tick(2.5, 2.5), "2.5");
        assert_eq!(fmt_tick(0.0, 1.0), "0");
        assert_eq!(fmt_tick(1e17, 1e16), "1e17");
    }

    #[test]
    fn wrapping_does_not_split_xml_entities() {
        let raw = "&<🍊".repeat(120);
        let lines = wrap_pending(&raw);
        assert!(lines.iter().all(|s| s.chars().count() <= 88));
        assert_eq!(lines.iter().map(|s| esc(s)).collect::<String>(), esc(&raw));
    }

    #[test]
    fn grouped_renderer_keeps_pending_visible_without_coordinates() {
        use crate::frontier::grouped::{GroupedFrontierPoint, GroupedFrontierResponse};
        use std::collections::BTreeMap;
        let pending = GroupedFrontierPoint {
            id: "group-pending".into(),
            attributes: BTreeMap::new(),
            label: "waiting".into(),
            color: "#268bd2".into(),
            investigations: vec!["i".into()],
            included: vec![],
            excluded: vec![],
            preliminary: true,
            values: None,
            on_frontier: None,
            dominated_by: vec![],
        };
        let svg = render_grouped(
            &GroupedFrontierResponse {
                points: vec![pending],
            },
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(svg.contains("no groups have a complete cohort"));
        assert!(svg.contains("pending / preliminary backlog"));
        assert!(svg.contains("members=1; included=0"));
        assert!(!svg.contains("NaN"));
    }

    #[test]
    fn grouped_renderer_handles_extreme_signed_finite_coordinates() {
        use crate::frontier::grouped::{GroupedFrontierPoint, GroupedFrontierResponse};
        use std::collections::BTreeMap;
        let make = |id: &str, x: f64, y: f64| GroupedFrontierPoint {
            id: id.into(),
            attributes: BTreeMap::new(),
            label: id.into(),
            color: "#268bd2".into(),
            investigations: vec![id.into()],
            included: vec![id.into()],
            excluded: vec![],
            preliminary: false,
            values: Some([("x".into(), x), ("y".into(), y)].into()),
            on_frontier: Some(true),
            dominated_by: vec![],
        };
        let svg = render_grouped(
            &GroupedFrontierResponse {
                points: vec![
                    make("group-low", -1e308, 1e308),
                    make("group-high", 1e308, -1e308),
                ],
            },
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(!svg.contains("NaN") && !svg.contains("inf"));
    }

    #[test]
    fn grouped_renderer_has_full_plot_and_subdues_whole_preliminary_marker() {
        use crate::frontier::grouped::{GroupedFrontierPoint, GroupedFrontierResponse};
        let point = GroupedFrontierPoint {
            id: "group-1234567890abcdef".into(),
            attributes: [("label".into(), Some("<\u{1}über-long".into()))].into(),
            label: "model-with-a-very-long-hash".into(),
            color: "#268bd2".into(),
            investigations: vec!["i".into(), "needs-grade".into()],
            included: vec!["i".into()],
            excluded: vec![crate::frontier::grouped::GroupExclusion {
                investigation: "needs-grade".into(),
                status: "awaiting_grades".into(),
                missing_grades: vec!["x".into()],
                missing_axes: vec![],
            }],
            preliminary: true,
            values: Some([("x".into(), 1.0), ("y".into(), 2.0)].into()),
            on_frontier: Some(true),
            dominated_by: vec![],
        };
        let svg = render_grouped(
            &GroupedFrontierResponse {
                points: vec![point],
            },
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(svg.contains("up &amp; right is better"));
        // Structural colors, including filled-point outlines, inherit the UI
        // theme. Point identity colors and preliminary semantics do not change.
        for color in [
            PAGE_BG,
            PANEL_BG,
            GRID_COLOR,
            LABEL_COLOR,
            TICK_COLOR,
            DIM_LABEL_COLOR,
        ] {
            assert!(svg.contains(color), "missing theme role: {color}");
        }
        assert!(svg.contains(r##"fill="#268bd2""##));
        assert!(svg.contains("rotate(-90)"));
        assert!(svg.contains("<path d=\"M ")); // staircase
        assert!(svg.contains("frontier-group preliminary"));
        assert!(svg.contains(".frontier-group:hover"));
        assert!(svg.contains(".frontier-group:focus"));
        assert!(svg.contains("data-group-id=\"group-1234567890abcdef\""));
        assert!(svg.contains("frontier; preliminary; x=1; y=2"));
        assert!(svg.contains("stroke-dasharray=\"3 2\""));
        assert!(svg.contains("class=\"legend-label\""));
        assert!(svg.contains("class=\"hit-target\""));
        assert!(!svg.contains("class=\"point-label\""));
        assert!(svg.contains("model-with-a-very-long-hash"));
        assert!(svg.contains("needs-grade"));
        assert!(svg.contains("(awaiting_grades)"));
        assert!(svg.contains("grades=x"));
        assert!(svg.contains("&lt;über-long"));
        assert!(!svg.contains('\u{1}'));
    }

    #[test]
    fn legend_width_preserves_complete_ascii_and_wide_unicode_labels() {
        use crate::frontier::grouped::GroupedFrontierPoint;
        let make = |id: &str, label: String| GroupedFrontierPoint {
            id: id.into(),
            attributes: Default::default(),
            label,
            color: "#268bd2".into(),
            investigations: vec![],
            included: vec![],
            excluded: vec![],
            preliminary: false,
            values: None,
            on_frontier: None,
            dominated_by: vec![],
        };
        let ascii = make("a", "a".repeat(60));
        let wide = make("b", "界".repeat(40));
        let width = legend_column_width(&[ascii, wide]);
        assert!(width >= 40.0 * 2.0 * 6.8 + 32.0);
    }

    #[test]
    fn legend_uses_principal_component_order_in_screen_space() {
        use crate::frontier::grouped::GroupedFrontierPoint;
        let make = |id: &str| GroupedFrontierPoint {
            id: id.into(),
            attributes: Default::default(),
            label: id.into(),
            color: "#268bd2".into(),
            investigations: vec![id.into()],
            included: vec![id.into()],
            excluded: vec![],
            preliminary: false,
            values: None,
            on_frontier: Some(true),
            dominated_by: vec![],
        };
        let a = make("a");
        let b = make("b");
        let c = make("c");
        // Higher y maps upward, so these raw values form a top-left to
        // bottom-right line in screen space. Input order is deliberately mixed.
        let usable = vec![(&c, 10.0, 0.0), (&a, 0.0, 10.0), (&b, 5.0, 5.0)];
        let (ordered, axis) = ordered_legend(
            &usable,
            &[],
            0.0,
            10.0,
            0.0,
            10.0,
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(axis.is_some());
        assert_eq!(
            ordered.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );

        // Lower-is-better x is inverted before PCA; visual order is still
        // left-to-right. A vertical cloud is deterministically top-to-bottom.
        let lower_x = vec![(&c, 0.0, 0.0), (&a, 10.0, 10.0), (&b, 5.0, 5.0)];
        let (ordered, axis) = ordered_legend(
            &lower_x,
            &[],
            0.0,
            10.0,
            0.0,
            10.0,
            &PlotAxis::new("x", BetterDirection::Lower),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(axis.is_some());
        assert_eq!(
            ordered.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        let vertical = vec![(&c, 5.0, 0.0), (&b, 5.0, 5.0), (&a, 5.0, 10.0)];
        let (ordered, axis) = ordered_legend(
            &vertical,
            &[],
            0.0,
            10.0,
            0.0,
            10.0,
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(axis.is_some());
        assert_eq!(
            ordered.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn pca_order_preserves_rendered_aspect_ratio() {
        use crate::frontier::grouped::GroupedFrontierPoint;
        let make = |id: &str| GroupedFrontierPoint {
            id: id.into(),
            attributes: Default::default(),
            label: id.into(),
            color: "#268bd2".into(),
            investigations: vec![id.into()],
            included: vec![id.into()],
            excluded: vec![],
            preliminary: false,
            values: None,
            on_frontier: Some(true),
            dominated_by: vec![],
        };
        let kh = make("keyboard-high");
        let kl = make("keyboard-low");
        let lh = make("lamp-high");
        let ll = make("lamp-low");
        let mh = make("mug-high");
        let ml = make("mug-low");
        let sh = make("socks-high");
        let sl = make("socks-low");
        let usable = vec![
            (&kh, 281.0, 427.0),
            (&kl, 245.0, 550.0),
            (&lh, 310.0, 163.0),
            (&ll, 239.0, 223.0),
            (&mh, 316.0, 194.0),
            (&ml, 186.0, 202.0),
            (&sh, 278.0, 487.0),
            (&sl, 175.0, 716.0),
        ];
        let (ordered, axis) = ordered_legend(
            &usable,
            &[],
            150.0,
            350.0,
            0.0,
            800.0,
            &PlotAxis::new("x", BetterDirection::Lower),
            &PlotAxis::new("y", BetterDirection::Lower),
        );
        assert!(axis.is_some());
        let ids = ordered.iter().map(|p| p.id.as_str()).collect::<Vec<_>>();
        assert_eq!(&ids[..2], &["mug-high", "lamp-high"]);
    }

    #[test]
    fn legend_adds_columns_instead_of_shrinking_plot() {
        use crate::frontier::grouped::{GroupedFrontierPoint, GroupedFrontierResponse};
        let points = (0..18)
            .map(|i| GroupedFrontierPoint {
                id: format!("group-{i:02}"),
                attributes: Default::default(),
                label: format!("variant-{i:02}"),
                color: "#268bd2".into(),
                investigations: vec![format!("i-{i}")],
                included: vec![format!("i-{i}")],
                excluded: vec![],
                preliminary: false,
                values: Some([("x".into(), i as f64), ("y".into(), (18 - i) as f64)].into()),
                on_frontier: Some(true),
                dominated_by: vec![],
            })
            .collect();
        let svg = render_grouped(
            &GroupedFrontierResponse { points },
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(svg.contains("width=\"1222\""));
        assert!(svg.contains("data-legend-column=\"1\""));
        assert!(svg.contains("class=\"pca-helper\""));
        assert!(svg.contains("legend PCA"));
        assert_eq!(svg.matches("class=\"legend-entry\"").count(), 18);
        assert!(!svg.contains("legend-label-clip"));
        assert!(svg.contains("variant-17"));
    }

    #[test]
    fn malformed_value_map_remains_visible_as_pending_evidence() {
        use crate::frontier::grouped::{GroupedFrontierPoint, GroupedFrontierResponse};
        let point = GroupedFrontierPoint {
            id: "group-malformed".into(),
            attributes: Default::default(),
            label: "malformed".into(),
            color: "#268bd2".into(),
            investigations: vec!["i".into()],
            included: vec!["i".into()],
            excluded: vec![],
            preliminary: false,
            values: Some([("x".into(), 1.0)].into()),
            on_frontier: Some(true),
            dominated_by: vec![],
        };
        let svg = render_grouped(
            &GroupedFrontierResponse {
                points: vec![point],
            },
            &PlotAxis::new("x", BetterDirection::Higher),
            &PlotAxis::new("y", BetterDirection::Higher),
        );
        assert!(svg.contains("frontier-group pending"));
        assert!(svg.contains("malformed"));
        assert!(svg.contains("pending / preliminary backlog"));
    }
}
