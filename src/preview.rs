//! A lightweight footprint preview. This is a comparison aid, not a KiCad renderer.
use crate::sexpr::{self, Node};
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

#[derive(Default)]
pub struct Preview {
    paths: Vec<Path>,
    labels: Vec<([f32; 2], String)>,
    bounds: Option<[f32; 4]>,
    pub warnings: Vec<String>,
}
struct Path {
    points: Vec<[f32; 2]>,
    closed: bool,
    fill: bool,
    color: Color32,
}
const PAD: Color32 = Color32::from_rgb(208, 164, 78);
const OUTLINE: Color32 = Color32::from_rgb(120, 194, 224);
const HOLE: Color32 = Color32::from_rgb(30, 33, 38);
fn number(n: &Node, index: usize) -> Option<f32> {
    n.children()
        .get(index)?
        .atom()?
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= 100_000.0)
}
fn xy(n: &Node) -> Option<[f32; 2]> {
    Some([number(n, 1)?, number(n, 2)?])
}
fn point(n: &Node, field: &str) -> Option<[f32; 2]> {
    xy(n.field(field)?)
}
fn rotate(p: [f32; 2], center: [f32; 2], angle: f32) -> [f32; 2] {
    let (s, c) = angle.to_radians().sin_cos();
    // KiCad pad rotation is counter-clockwise in its downward-positive Y coordinates.
    [
        center[0] + p[0] * c + p[1] * s,
        center[1] - p[0] * s + p[1] * c,
    ]
}
fn ellipse(center: [f32; 2], size: [f32; 2], angle: f32) -> Vec<[f32; 2]> {
    (0..64)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 64.0;
            rotate(
                [a.cos() * size[0] / 2.0, a.sin() * size[1] / 2.0],
                center,
                angle,
            )
        })
        .collect()
}
fn rounded(center: [f32; 2], size: [f32; 2], radius: f32, angle: f32) -> Vec<[f32; 2]> {
    let mut points = Vec::new();
    for (cx, cy, start) in [
        (1.0, 1.0, 0.0),
        (-1.0, 1.0, 90.0),
        (-1.0, -1.0, 180.0),
        (1.0, -1.0, 270.0),
    ] {
        for i in 0..=12 {
            let a = (start + i as f32 * 7.5).to_radians();
            points.push(rotate(
                [
                    cx * (size[0] / 2.0 - radius) + radius * a.cos(),
                    cy * (size[1] / 2.0 - radius) + radius * a.sin(),
                ],
                center,
                angle,
            ));
        }
    }
    points
}
impl Preview {
    pub fn from_raw(raw: &str) -> Self {
        let mut preview = Self::default();
        let root = match sexpr::parse(raw) {
            Ok(n) => n,
            Err(e) => {
                preview
                    .warnings
                    .push(format!("Cannot parse footprint: {e}"));
                return preview;
            }
        };
        if !matches!(root.head(), Some("footprint" | "module")) {
            preview
                .warnings
                .push("Expected a footprint or module".into());
            return preview;
        }
        for n in root.children() {
            let valid = match n.head() {
                Some("pad") => preview.pad(n),
                Some("fp_line") => preview.line(n),
                Some("fp_rect") => preview.rect(n),
                Some("fp_circle") => preview.circle(n),
                Some("fp_poly") => preview.poly(n),
                Some("fp_arc") => preview.arc(n),
                Some("fp_curve") => {
                    preview
                        .warnings
                        .push("Bezier curve omitted from preview".into());
                    true
                }
                _ => true,
            };
            if !valid {
                preview.warnings.push(format!(
                    "Invalid {} omitted from preview",
                    n.head().unwrap_or("geometry")
                ));
            }
        }
        if preview.bounds.is_none() {
            preview
                .warnings
                .push("No drawable footprint geometry".into());
        }
        preview
    }
    fn add(&mut self, points: Vec<[f32; 2]>, closed: bool, fill: bool, color: Color32) {
        if points.is_empty()
            || points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 200_000.0)
        {
            return;
        }
        for p in &points {
            let b = self.bounds.get_or_insert([p[0], p[1], p[0], p[1]]);
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
        }
        self.paths.push(Path {
            points,
            closed,
            fill,
            color,
        });
    }
    fn pad(&mut self, n: &Node) -> bool {
        let Some(at) = n.field("at") else {
            return false;
        };
        let Some(center) = xy(at) else { return false };
        let angle = if at.children().len() > 3 {
            let Some(a) = number(at, 3) else { return false };
            a
        } else {
            0.0
        };
        let Some(size) = point(n, "size").filter(|s| s[0] > 0.0 && s[1] > 0.0) else {
            return false;
        };
        let shape = n.children().get(3).and_then(Node::atom).unwrap_or("");
        let points = match shape {
            "circle" => ellipse(center, size, angle),
            "rect" => rounded(center, size, 0.0, angle),
            "oval" => rounded(center, size, size[0].min(size[1]) / 2.0, angle),
            "roundrect" => {
                let ratio = n
                    .field("roundrect_rratio")
                    .and_then(|r| number(r, 1))
                    .unwrap_or(0.25);
                if !(0.0..=0.5).contains(&ratio) {
                    return false;
                }
                rounded(center, size, size[0].min(size[1]) * ratio, angle)
            }
            _ => {
                self.warnings.push(format!(
                    "Unsupported pad shape '{shape}'; bounding rectangle shown"
                ));
                rounded(center, size, 0.0, angle)
            }
        };
        self.add(points, true, true, PAD);
        if let Some(drill) = n.field("drill") {
            let oval = drill.children().get(1).and_then(Node::atom) == Some("oval");
            let d = number(drill, if oval { 2 } else { 1 });
            if let Some(d) = d.filter(|d| *d > 0.0) {
                let dy = if oval {
                    number(drill, 3).unwrap_or(d)
                } else {
                    d
                };
                if dy > 0.0 {
                    let offset = drill.field("offset").and_then(xy).unwrap_or([0.0, 0.0]);
                    self.add(
                        rounded(
                            rotate(offset, center, angle),
                            [d, dy],
                            d.min(dy) / 2.0,
                            angle,
                        ),
                        true,
                        true,
                        HOLE,
                    );
                } else {
                    self.warnings.push("Invalid drill omitted".into());
                }
            } else {
                self.warnings.push("Invalid drill omitted".into());
            }
        }
        self.labels
            .push((center, n.value().unwrap_or("").to_owned()));
        true
    }
    fn line(&mut self, n: &Node) -> bool {
        let (Some(a), Some(b)) = (point(n, "start"), point(n, "end")) else {
            return false;
        };
        self.add(vec![a, b], false, false, OUTLINE);
        true
    }
    fn rect(&mut self, n: &Node) -> bool {
        let (Some(a), Some(b)) = (point(n, "start"), point(n, "end")) else {
            return false;
        };
        self.add(vec![a, [b[0], a[1]], b, [a[0], b[1]]], true, false, OUTLINE);
        true
    }
    fn circle(&mut self, n: &Node) -> bool {
        let (Some(c), Some(e)) = (point(n, "center"), point(n, "end")) else {
            return false;
        };
        let d = 2.0 * (e[0] - c[0]).hypot(e[1] - c[1]);
        self.add(ellipse(c, [d, d], 0.0), true, false, OUTLINE);
        true
    }
    fn poly(&mut self, n: &Node) -> bool {
        let Some(pts) = n.field("pts") else {
            return false;
        };
        let Some(points) = pts
            .children()
            .iter()
            .skip(1)
            .map(xy)
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        if points.len() < 2 {
            return false;
        }
        self.add(points, true, false, OUTLINE);
        true
    }
    fn arc(&mut self, n: &Node) -> bool {
        let (Some(a), Some(m), Some(b)) = (point(n, "start"), point(n, "mid"), point(n, "end"))
        else {
            self.warnings.push("Legacy arc omitted from preview".into());
            return true;
        };
        let determinant =
            2.0 * (a[0] * (m[1] - b[1]) + m[0] * (b[1] - a[1]) + b[0] * (a[1] - m[1]));
        if determinant.abs() < 1e-6 {
            return false;
        }
        let q = |p: [f32; 2]| p[0] * p[0] + p[1] * p[1];
        let center = [
            (q(a) * (m[1] - b[1]) + q(m) * (b[1] - a[1]) + q(b) * (a[1] - m[1])) / determinant,
            (q(a) * (b[0] - m[0]) + q(m) * (a[0] - b[0]) + q(b) * (m[0] - a[0])) / determinant,
        ];
        let angle = |p: [f32; 2]| (p[1] - center[1]).atan2(p[0] - center[0]);
        let start = angle(a);
        let end = (angle(b) - start).rem_euclid(std::f32::consts::TAU);
        let middle = (angle(m) - start).rem_euclid(std::f32::consts::TAU);
        let sweep = if middle <= end {
            end
        } else {
            end - std::f32::consts::TAU
        };
        let radius = (a[0] - center[0]).hypot(a[1] - center[1]);
        self.add(
            (0..=64)
                .map(|i| {
                    let t = start + sweep * i as f32 / 64.0;
                    [center[0] + radius * t.cos(), center[1] + radius * t.sin()]
                })
                .collect(),
            false,
            false,
            OUTLINE,
        );
        true
    }
    pub fn span(&self) -> [f32; 2] {
        let b = self.bounds.unwrap_or([-0.5, -0.5, 0.5, 0.5]);
        [(b[2] - b[0]).max(0.001), (b[3] - b[1]).max(0.001)]
    }
    pub fn paint(&self, ui: &mut egui::Ui, scale: f32) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().max(1.0), 300.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, HOLE);
        let b = self.bounds.unwrap_or([-0.5, -0.5, 0.5, 0.5]);
        let scale = if scale.is_finite() && scale > 0.0 {
            scale.min(100_000.0)
        } else {
            1.0
        };
        let transform = |p: [f32; 2]| {
            Pos2::new(
                rect.center().x + (p[0] - (b[0] + b[2]) / 2.0) * scale,
                rect.center().y + (p[1] - (b[1] + b[3]) / 2.0) * scale,
            )
        };
        for path in &self.paths {
            let mut points: Vec<_> = path.points.iter().copied().map(transform).collect();
            if path.fill {
                painter.add(egui::Shape::convex_polygon(
                    points,
                    path.color,
                    Stroke::NONE,
                ));
            } else {
                if path.closed {
                    points.push(points[0]);
                }
                painter.add(egui::Shape::line(points, Stroke::new(1.5, path.color)));
            }
        }
        for (p, label) in &self.labels {
            painter.text(
                transform(*p),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
        let span = self.span();
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.2} × {:.2} mm", span[0], span[1]),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
        );
        response
    }
}
pub fn fit_scale(left: &Preview, right: &Preview, width: f32, height: f32) -> f32 {
    let a = left.span();
    let b = right.span();
    ((width - 32.0).max(1.0) / a[0].max(b[0]))
        .min((height - 40.0).max(1.0) / a[1].max(b[1]))
        .clamp(0.0001, 100_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotated_asymmetric_pad_and_unicode() {
        let p = Preview::from_raw(
            r#"(footprint "µ (quoted)" (pad "Ω" smd rect (at 2 3 90) (size 4 2)))"#,
        );
        assert!(p.warnings.is_empty());
        assert_eq!(p.labels[0].1, "Ω");
        let span = p.span();
        assert!((span[0] - 2.0).abs() < 0.001);
        assert!((span[1] - 4.0).abs() < 0.001);
    }
    #[test]
    fn malformed_and_nonfinite_geometry_is_safe() {
        for input in [
            "(footprint",
            "(footprint x (pad 1 smd rect (at NaN 0) (size 2 2)))",
            "(footprint x (pad 1 smd rect (at 0 0) (size -1 2)))",
            "(footprint x (fp_line (start inf 0) (end 1 1)))",
        ] {
            let p = Preview::from_raw(input);
            assert!(!p.warnings.is_empty());
            assert!(p.span().iter().all(|n| n.is_finite() && *n > 0.0));
        }
    }
    #[test]
    fn unknown_shapes_and_drill_offsets_are_visible() {
        let p = Preview::from_raw(
            "(footprint x (pad 1 thru_hole custom (at 0 0) (size 2 2) (drill 1 (offset 0.2 0))))",
        );
        assert!(p.warnings.iter().any(|w| w.contains("custom")));
        assert_eq!(p.paths.len(), 2);
    }
    #[test]
    fn outlines_and_arcs_have_expected_bounds() {
        let p = Preview::from_raw(
            "(footprint x (fp_rect (start -2 -3) (end 2 3)) (fp_circle (center 0 0) (end 1 0)) (fp_arc (start 1 0) (mid 0 1) (end -1 0)) (fp_poly (pts (xy 0 0) (xy 1 0) (xy 0 1))))",
        );
        assert!(p.warnings.is_empty());
        assert_eq!(p.span(), [4.0, 6.0]);
        assert_eq!(p.paths.len(), 4);
    }
    #[test]
    fn renders_without_a_display() {
        let p = Preview::from_raw("(footprint x (pad 1 smd oval (at 0 0 45) (size 2 4)))");
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            p.paint(ui, fit_scale(&p, &p, 300.0, 300.0));
        });
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }
}
