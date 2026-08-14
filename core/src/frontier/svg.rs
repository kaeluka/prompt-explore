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

const TICK_COLOR: &str = "#8a919e";
const LABEL_COLOR: &str = "#c8cdd6";
const DIM_LABEL_COLOR: &str = "#7c8593";
const GRID_COLOR: &str = "#232734";
const PANEL_BG: &str = "#0f1115";
const PAGE_BG: &str = "#16181d";
const FRONTIER_STROKE: &str = "#7c6cff";

/// XML-escape a text run. & < > " ' — the five predefined entities.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
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
    let t = ((v - plo) / (phi - plo)).clamp(0.0, 1.0);
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
        let ly = if cy - 20.0 < T + 8.0 { cy + 20.0 } else { cy - 9.0 };
        s.push_str(&format!(
            r#"<text x="{lx:.1}" y="{ly:.1}" font-size="11" fill="{fill}" text-anchor="{anchor}">{}</text>"#,
            esc(&p.label)
        ));
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
}
