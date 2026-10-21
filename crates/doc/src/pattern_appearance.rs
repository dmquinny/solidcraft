//! Procedural surface patterns of a look (a self-hosting patch, not upstream): wood grain and
//! brushed, blasted or mottled finishes, which the viewport works out from each surface point,
//! so a look needs no texture file. Saved inside the look; a look without one is plain colour.

use serde::{Deserialize, Serialize};

/// What the pattern is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Growth rings and fibres along the grain (`scale_mm`: the ring spacing).
    Wood,
    /// Long thin streaks along the grain (`scale_mm`: the streak width).
    Brushed,
    /// An even speckle: bead blasted, anodized, cast (`scale_mm`: the speckle size).
    Blasted,
    /// Patches of the second colour: patina, rust, wear (`scale_mm`: the patch size).
    Mottled,
}

impl PatternKind {
    pub const ALL: [PatternKind; 4] = [PatternKind::Wood, PatternKind::Brushed, PatternKind::Blasted, PatternKind::Mottled];

    /// The viewport's number for it (0 is no pattern).
    pub fn code(self) -> u8 {
        match self {
            PatternKind::Wood => 1,
            PatternKind::Brushed => 2,
            PatternKind::Blasted => 3,
            PatternKind::Mottled => 4,
        }
    }
}

/// The grain's direction: along the body's longest side (`auto`), or a world axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternAxis {
    #[default]
    Auto,
    X,
    Y,
    Z,
}

/// Smallest and largest pattern scale (mm).
pub const SCALE_MM: std::ops::RangeInclusive<f64> = 0.25..=60.0;

/// A look's pattern. Construction and loading share the same bounds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Wire", into = "Wire")]
pub struct Pattern {
    kind: PatternKind,
    scale_mm: f64,
    contrast: f64,
    color: [u8; 3],
    axis: PatternAxis,
    seed: u8,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    kind: PatternKind,
    scale_mm: f64,
    contrast: f64,
    color: [u8; 3],
    #[serde(default, skip_serializing_if = "is_auto")]
    axis: PatternAxis,
    #[serde(default, skip_serializing_if = "is_zero")]
    seed: u8,
}

fn is_auto(a: &PatternAxis) -> bool {
    *a == PatternAxis::Auto
}

fn is_zero(s: &u8) -> bool {
    *s == 0
}

impl Pattern {
    /// `scale_mm` within [`SCALE_MM`], `contrast` 0–1; `color` is the second colour (latewood,
    /// patina, rust); `seed` varies the pattern between bodies that share the look.
    pub fn new(kind: PatternKind, scale_mm: f64, contrast: f64, color: [u8; 3], axis: PatternAxis, seed: u8) -> Result<Self, String> {
        if !scale_mm.is_finite() || !SCALE_MM.contains(&scale_mm) {
            return Err(format!("pattern scale_mm must be {}–{} mm", SCALE_MM.start(), SCALE_MM.end()));
        }
        if !contrast.is_finite() || !(0.0..=1.0).contains(&contrast) {
            return Err("pattern contrast must be 0–1".into());
        }
        Ok(Self { kind, scale_mm, contrast, color, axis, seed })
    }

    pub fn kind(self) -> PatternKind {
        self.kind
    }
    pub fn scale_mm(self) -> f64 {
        self.scale_mm
    }
    pub fn contrast(self) -> f64 {
        self.contrast
    }
    pub fn color(self) -> [u8; 3] {
        self.color
    }
    pub fn axis(self) -> PatternAxis {
        self.axis
    }
    pub fn seed(self) -> u8 {
        self.seed
    }

    /// The viewport's packed form: kind, grain axis (`axis`: 0 x, 1 y, 2 z, already resolved),
    /// scale in quarter millimetres and contrast (0–255).
    pub fn gpu_bytes(self, axis: u8) -> [u8; 4] {
        let scale = (self.scale_mm * 4.0).round().clamp(1.0, 255.0) as u8;
        let contrast = (self.contrast * 255.0).round().clamp(0.0, 255.0) as u8;
        [self.kind.code(), axis.min(2), scale, contrast]
    }
}

impl TryFrom<Wire> for Pattern {
    type Error = String;
    fn try_from(w: Wire) -> Result<Self, Self::Error> {
        Self::new(w.kind, w.scale_mm, w.contrast, w.color, w.axis, w.seed)
    }
}

impl From<Pattern> for Wire {
    fn from(p: Pattern) -> Self {
        Self { kind: p.kind, scale_mm: p.scale_mm, contrast: p.contrast, color: p.color, axis: p.axis, seed: p.seed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patterns_round_trip_and_refuse_what_they_cannot_draw() {
        let p = Pattern::new(PatternKind::Wood, 5.0, 0.8, [150, 96, 62], PatternAxis::Auto, 0).unwrap();
        let saved = serde_json::to_value(p).unwrap();
        assert_eq!(saved, json!({"kind": "wood", "scale_mm": 5.0, "contrast": 0.8, "color": [150, 96, 62]}));
        assert_eq!(serde_json::from_value::<Pattern>(saved.clone()).unwrap(), p);
        let z = Pattern::new(PatternKind::Brushed, 0.5, 0.4, [1, 2, 3], PatternAxis::Z, 7).unwrap();
        assert_eq!(serde_json::from_value::<Pattern>(serde_json::to_value(z).unwrap()).unwrap(), z);
        for (key, value) in [
            ("scale_mm", json!(0.1)),
            ("scale_mm", json!(61)),
            ("contrast", json!(-0.1)),
            ("contrast", json!(1.5)),
            ("kind", json!("plaid")),
            ("axis", json!("w")),
            ("color", json!([1, 2])),
            ("color", json!([1, 2, 300])),
            ("seed", json!(256)),
            ("extra", json!(true)),
        ] {
            let mut bad = saved.clone();
            bad[key] = value;
            assert!(serde_json::from_value::<Pattern>(bad).is_err(), "{key} accepted");
        }
        assert!(Pattern::new(PatternKind::Wood, f64::NAN, 0.5, [0; 3], PatternAxis::Auto, 0).is_err());
        assert!(Pattern::new(PatternKind::Wood, 5.0, f64::INFINITY, [0; 3], PatternAxis::Auto, 0).is_err());
    }

    #[test]
    fn the_viewport_gets_quarter_millimetres_and_a_resolved_axis() {
        let p = Pattern::new(PatternKind::Mottled, 60.0, 1.0, [0; 3], PatternAxis::Auto, 3).unwrap();
        assert_eq!(p.gpu_bytes(2), [4, 2, 240, 255]);
        let tiny = Pattern::new(PatternKind::Blasted, 0.25, 0.0, [0; 3], PatternAxis::X, 0).unwrap();
        assert_eq!(tiny.gpu_bytes(9), [3, 2, 1, 0]);
    }
}
