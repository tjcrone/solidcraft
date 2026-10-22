//! The toolpaths a slice produced, as a layer preview draws them: layers of extruding paths,
//! each with the feature it prints (outer wall, sparse infill, support…), its bead width and
//! speed, and its points in millimetres on the plate. This is the engine's record of what it
//! wrote the G-code from, taken in the same slice, not a reading of the G-code file.
//!
//! The travel moves are not stored: within a layer the head travels from the last point of one
//! path to the first point of the next ([`Layer::travels`]).

/// What a path prints, in the order the slicer's `;TYPE:` labels name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Feature {
    OuterWall,
    InnerWall,
    OverhangWall,
    TopSurface,
    BottomSurface,
    InternalSolid,
    SparseInfill,
    Bridge,
    InternalBridge,
    Support,
    SupportInterface,
    Brim,
    Skirt,
    Ironing,
    GapFill,
    PrimeTower,
    Custom,
}

impl Feature {
    /// Every feature, in display order.
    pub const ALL: [Feature; 17] = [
        Feature::OuterWall,
        Feature::InnerWall,
        Feature::OverhangWall,
        Feature::TopSurface,
        Feature::BottomSurface,
        Feature::InternalSolid,
        Feature::SparseInfill,
        Feature::Bridge,
        Feature::InternalBridge,
        Feature::Support,
        Feature::SupportInterface,
        Feature::Brim,
        Feature::Skirt,
        Feature::Ironing,
        Feature::GapFill,
        Feature::PrimeTower,
        Feature::Custom,
    ];

    /// The stable lower-case id for JSON and settings (`outer_wall`, `sparse_infill`, …).
    pub fn id(self) -> &'static str {
        match self {
            Feature::OuterWall => "outer_wall",
            Feature::InnerWall => "inner_wall",
            Feature::OverhangWall => "overhang_wall",
            Feature::TopSurface => "top_surface",
            Feature::BottomSurface => "bottom_surface",
            Feature::InternalSolid => "internal_solid_infill",
            Feature::SparseInfill => "sparse_infill",
            Feature::Bridge => "bridge",
            Feature::InternalBridge => "internal_bridge",
            Feature::Support => "support",
            Feature::SupportInterface => "support_interface",
            Feature::Brim => "brim",
            Feature::Skirt => "skirt",
            Feature::Ironing => "ironing",
            Feature::GapFill => "gap_fill",
            Feature::PrimeTower => "prime_tower",
            Feature::Custom => "custom",
        }
    }

    /// The English name shown in a legend.
    pub fn label(self) -> &'static str {
        match self {
            Feature::OuterWall => "Outer wall",
            Feature::InnerWall => "Inner wall",
            Feature::OverhangWall => "Overhang wall",
            Feature::TopSurface => "Top surface",
            Feature::BottomSurface => "Bottom surface",
            Feature::InternalSolid => "Internal solid infill",
            Feature::SparseInfill => "Sparse infill",
            Feature::Bridge => "Bridge",
            Feature::InternalBridge => "Internal bridge",
            Feature::Support => "Support",
            Feature::SupportInterface => "Support interface",
            Feature::Brim => "Brim",
            Feature::Skirt => "Skirt",
            Feature::Ironing => "Ironing",
            Feature::GapFill => "Gap fill",
            Feature::PrimeTower => "Prime tower",
            Feature::Custom => "Custom",
        }
    }

    /// The feature with this [`id`](Self::id), if any.
    pub fn parse(id: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.id() == id)
    }
}

/// One extruding path: a polyline the head prints without lifting. A closed loop repeats its
/// first point at the end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub feature: Option<Feature>,
    /// 1-based filament slot.
    pub tool: u8,
    pub width_mm: f32,
    pub speed_mm_s: f32,
    /// Points on the plate, millimetres; at least two.
    pub points: Vec<[f32; 2]>,
}

impl Path {
    /// The printed length of the path, millimetres.
    pub fn length_mm(&self) -> f64 {
        self.points.windows(2).map(|w| f64::from((w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))).sum()
    }

    /// Straight segments the path is made of.
    pub fn segments(&self) -> usize {
        self.points.len().saturating_sub(1)
    }
}

/// One layer: its top height, its thickness and the paths it prints in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    /// Top of the layer, millimetres.
    pub z: f32,
    pub height: f32,
    /// Estimated time to print the layer, seconds.
    pub time_s: f32,
    pub paths: Vec<Path>,
}

impl Layer {
    /// The travel moves between the layer's paths: from the end of each path to the start of the
    /// next.
    pub fn travels(&self) -> impl Iterator<Item = ([f32; 2], [f32; 2])> + '_ {
        self.paths.windows(2).filter_map(|w| Some((*w[0].points.last()?, *w[1].points.first()?)))
    }

    pub fn segments(&self) -> usize {
        self.paths.iter().map(Path::segments).sum()
    }
}

/// A bead the file commands outside the sliced layers (a purge line in the start template, a
/// wipe in the end template), read back from the G-code: points in plate millimetres with
/// their Z, and the bead's width worked out from what was extruded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileBead {
    pub points: Vec<[f32; 3]>,
    pub width_mm: f32,
}

impl FileBead {
    pub fn length_mm(&self) -> f64 {
        self.points.windows(2).map(|w| f64::from((w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))).sum()
    }
}

/// The extents of the toolpaths, millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

/// Totals for one feature over the whole print.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureTotal {
    pub feature: Option<Feature>,
    pub paths: usize,
    pub length_mm: f64,
}

/// Every layer of a slice, bottom first, and the beads the file adds around them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toolpaths {
    pub layers: Vec<Layer>,
    /// Beads from the start and end templates that land inside the bed (`crate::file_beads`).
    pub file_beads: Vec<FileBead>,
}

impl Toolpaths {
    pub fn segments(&self) -> usize {
        self.layers.iter().map(Layer::segments).sum()
    }

    /// The box the printed paths occupy (bead widths not counted), or `None` for no paths. Z runs
    /// from the bottom of the lowest layer to the top of the highest.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut b: Option<Bounds> = None;
        for l in &self.layers {
            let (bottom, top) = ((l.z - l.height).max(0.0), l.z);
            for p in l.paths.iter().flat_map(|p| p.points.iter()) {
                let b = b.get_or_insert(Bounds { min: [p[0], p[1], bottom], max: [p[0], p[1], top] });
                b.min = [b.min[0].min(p[0]), b.min[1].min(p[1]), b.min[2].min(bottom)];
                b.max = [b.max[0].max(p[0]), b.max[1].max(p[1]), b.max[2].max(top)];
            }
        }
        b
    }

    /// Paths and printed length per feature, in [`Feature::ALL`] order, features without a path
    /// left out; paths of no known feature last.
    pub fn by_feature(&self) -> Vec<FeatureTotal> {
        let mut totals: Vec<FeatureTotal> = Feature::ALL.iter().map(|f| FeatureTotal { feature: Some(*f), ..Default::default() }).collect();
        totals.push(FeatureTotal::default());
        for p in self.layers.iter().flat_map(|l| l.paths.iter()) {
            let k = p.feature.and_then(|f| Feature::ALL.iter().position(|x| *x == f)).unwrap_or(Feature::ALL.len());
            if let Some(t) = totals.get_mut(k) {
                t.paths += 1;
                t.length_mm += p.length_mm();
            }
        }
        totals.retain(|t| t.paths > 0);
        totals
    }
}
