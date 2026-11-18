//! 3D scenes and depth effects an extension component's home view draws (spec 3.3 §6.1
//! `genui`, extension catalogs): the home's own elements, like [`DrawnChart`], so the home and
//! any host drawing with `DrawnView` show them natively, and the terminal and the phone show
//! their words.
//!
//! A [`DrawnScene`] is a bounded, declarative scene: shapes, lines, instanced bars and points,
//! a height grid, a globe and labels. Its colours are the home's palette and state tokens,
//! never colours the agent picks. It has no textures, models, fonts, shaders or links, and
//! every string in it is checked by rule 5. Picking an object fills a choice field the drawing
//! shows; it never sends anything ([`SceneSelect`]).
//!
//! A [`DrawnDepth`] presents the components a home view holds with CSS 3D, without WebGL: a
//! card that tilts or flips, stacked layers, a carousel, a curved wall or an orbitable stage.
//!
//! [`DrawnChart`]: super::DrawnChart

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Number;

/// A point or a size in a scene, `[x, y, z]`; `y` is up.
pub type Xyz = [Number; 3];

fn origin() -> Xyz {
    [Number::from(0), Number::from(0), Number::from(0)]
}

/// A 3D scene a home view draws, as `HomeNode::Scene`.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{DrawnScene, SceneKind, SceneObject, SceneShape};
///
/// let scene = DrawnScene {
///     objects: vec![SceneObject::new(
///         "dock-3",
///         SceneShape::Box { size: [2.into(), 1.into(), 4.into()] },
///     )
///     .labelled("Dock 3")],
///     ..DrawnScene::new(SceneKind::Space)
/// };
/// let wire = serde_json::to_value(&scene).unwrap();
/// assert_eq!(wire["objects"][0]["shape"]["type"], "box");
/// assert_eq!(wire["camera"]["preset"], "fit");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnScene {
    /// What the scene is, for its summary and default camera.
    pub kind: SceneKind,
    /// Its objects, parents before children.
    pub objects: Vec<SceneObject>,
    /// Where it is seen from.
    #[serde(default)]
    pub camera: SceneCamera,
    /// How it is lit: a preset whose soft light always stays, so nothing goes dark.
    #[serde(default)]
    pub lighting: SceneLighting,
    /// Up to three more lights.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lights: Vec<SceneLight>,
    /// Named axes, ticks and a floor grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axes: Option<SceneAxes>,
    /// Objects that move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motion: Vec<SceneMotion>,
    /// Picking an object fills this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<SceneSelect>,
    /// Its height in CSS pixels, 200 to 960; the home picks one when it is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u16>,
    /// What bar and grid heights stand for in the data's own numbers, for the summary and the
    /// home's readouts; heights are the data's numbers when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<SceneValues>,
    /// One sentence that says what the scene shows, for screen readers and the terminal,
    /// written by the server: "3D bar chart of Revenue, 24 bars, from 3 to 98".
    #[serde(default)]
    pub summary: String,
    /// The held components shown beside objects: written by the server, each the key of one
    /// of the node's children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<ScenePanel>,
    /// The key of the node's child that says in words what the scene shows, written by the
    /// server: shown by "Show as text", where WebGL is missing, and in the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_view: Option<String>,
}

impl DrawnScene {
    /// An empty scene of `kind`, seen to fit, in the studio light.
    pub fn new(kind: SceneKind) -> Self {
        Self {
            kind,
            objects: Vec::new(),
            camera: SceneCamera::default(),
            lighting: SceneLighting::default(),
            lights: Vec::new(),
            axes: None,
            motion: Vec::new(),
            select: None,
            height: None,
            values: None,
            summary: String::new(),
            panels: Vec::new(),
            text_view: None,
        }
    }
}

/// How a scene's heights map to the data's numbers: a height `h` stands for
/// `h * scale + offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneValues {
    /// What one unit of height stands for; above 0.
    pub scale: Number,
    /// What a height of 0 stands for.
    #[serde(default = "zero")]
    pub offset: Number,
}

fn zero() -> Number {
    Number::from(0)
}

impl SceneValues {
    /// The data's number a height stands for.
    pub fn value(&self, height: f64) -> f64 {
        height * self.scale.as_f64().unwrap_or(1.0) + self.offset.as_f64().unwrap_or(0.0)
    }
}

/// What a scene is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneKind {
    /// Objects laid out in space: a floor plan, a rack, a machine.
    Space,
    /// Bars over two categories.
    Bars,
    /// Points over three measures.
    Points,
    /// A value over a grid.
    Surface,
    /// Nodes and the links between them.
    Network,
    /// Places on a globe and the routes between them.
    Globe,
}

/// One object of a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneObject {
    /// Unique within the scene: what parents, panels, motion and picking name it by.
    pub id: String,
    /// The group it belongs to: it moves, turns and scales with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// What it is called, shown beside it as page text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its shape.
    pub shape: SceneShape,
    /// Where it is, in its parent's space.
    #[serde(default = "origin")]
    pub at: Xyz,
    /// How it is turned about each axis, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Xyz>,
    /// How it is scaled along each axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Xyz>,
    /// How it looks.
    #[serde(default)]
    pub look: SceneLook,
    /// A state it is in, drawn in the state's colour beside its label's word; an object with
    /// a state has a label, so colour is never the only sign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SceneState>,
    /// The option value picking it chooses (see [`SceneSelect`]); an object with one has a
    /// label, so the person sees what they pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl SceneObject {
    /// An object `id` of `shape`, at the origin, in the first chart colour.
    pub fn new(id: impl Into<String>, shape: SceneShape) -> Self {
        Self {
            id: id.into(),
            parent: None,
            label: None,
            shape,
            at: origin(),
            turn: None,
            scale: None,
            look: SceneLook::default(),
            state: None,
            value: None,
        }
    }

    /// With `label`.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// At `at`.
    #[must_use]
    pub fn at(mut self, at: Xyz) -> Self {
        self.at = at;
        self
    }
}

/// A scene object's shape. Flat number lists hold one point after another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SceneShape {
    /// No shape of its own: it holds the objects whose parent it is.
    Group,
    /// A box.
    Box {
        /// Width, height and depth.
        size: Xyz,
    },
    /// A sphere.
    Sphere {
        /// Its radius.
        radius: Number,
    },
    /// A cylinder; a cone has a top radius of 0.
    Cylinder {
        /// The top's radius.
        radius_top: Number,
        /// The bottom's radius.
        radius_bottom: Number,
        /// Its height.
        height: Number,
    },
    /// A ring.
    Torus {
        /// From the centre to the middle of the tube.
        radius: Number,
        /// The tube's radius.
        tube: Number,
    },
    /// A flat rectangle on the ground.
    Plane {
        /// Along x.
        width: Number,
        /// Along z.
        depth: Number,
    },
    /// A line through points, `x, y, z` after one another.
    Line {
        /// The points, flat.
        points: Vec<Number>,
        /// Separate segments, each a pair of points, rather than one line through them all.
        #[serde(default)]
        segments: bool,
        /// A smooth curve through the points.
        #[serde(default)]
        smooth: bool,
        /// Arrowheads.
        #[serde(default)]
        arrow: LineEnds,
        /// Dashed.
        #[serde(default)]
        dashed: bool,
    },
    /// Bars standing on the ground, one per `x, z` pair.
    Bars {
        /// Where each bar stands, `x, z` flat.
        at: Vec<Number>,
        /// Each bar's height.
        heights: Vec<Number>,
        /// Each bar's width and depth.
        width: Number,
        /// Each bar's series, 0 to 7, drawn in that chart colour; empty for the object's own.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        series: Vec<u8>,
    },
    /// Points, `x, y, z` flat.
    Points {
        /// Where each point is, flat.
        at: Vec<Number>,
        /// Each point's radius.
        size: Number,
        /// Each point's series, 0 to 7, drawn in that chart colour; empty for the object's own.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        series: Vec<u8>,
    },
    /// A surface over a grid of heights, row after row, coloured by height.
    Grid {
        /// Points along x.
        columns: u16,
        /// Points along z.
        rows: u16,
        /// One height per point, row after row.
        heights: Vec<Number>,
        /// Along x.
        width: Number,
        /// Along z.
        depth: Number,
    },
    /// A globe with lines of latitude and longitude.
    Globe {
        /// Its radius.
        radius: Number,
    },
    /// Text facing the viewer, as page text.
    Label {
        /// What it reads.
        text: String,
        /// How large.
        #[serde(default)]
        size: LabelSize,
    },
}

/// A line's arrowheads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LineEnds {
    /// None.
    #[default]
    None,
    /// At the last point.
    End,
    /// At both ends.
    Both,
}

/// How large a label is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LabelSize {
    /// A caption.
    Small,
    /// Body text.
    #[default]
    Medium,
    /// A heading.
    Large,
}

/// How an object looks: the home's colours, never one the agent picks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneLook {
    /// Its colour.
    #[serde(default)]
    pub color: SceneColor,
    /// How opaque, 0.15 to 1; 1 when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<Number>,
    /// Drawn as edges only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wireframe: bool,
    /// Its surface.
    #[serde(default)]
    pub finish: SceneFinish,
}

/// A colour of the home's palette.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneColor {
    /// The first chart colour.
    #[default]
    Chart1,
    /// The second chart colour.
    Chart2,
    /// The third chart colour.
    Chart3,
    /// The fourth chart colour.
    Chart4,
    /// The fifth chart colour.
    Chart5,
    /// The sixth chart colour.
    Chart6,
    /// The seventh chart colour.
    Chart7,
    /// The eighth chart colour.
    Chart8,
    /// The text colour.
    Ink,
    /// A plain grey.
    Neutral,
    /// A faint grey.
    Muted,
    /// By height, low to high (a grid).
    Ramp,
}

impl SceneColor {
    /// The chart colour of series `index`, 0 to 7.
    pub fn chart(index: u8) -> Self {
        match index % 8 {
            0 => Self::Chart1,
            1 => Self::Chart2,
            2 => Self::Chart3,
            3 => Self::Chart4,
            4 => Self::Chart5,
            5 => Self::Chart6,
            6 => Self::Chart7,
            _ => Self::Chart8,
        }
    }
}

/// An object's surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneFinish {
    /// Soft.
    #[default]
    Matte,
    /// Shiny.
    Glossy,
    /// Unlit, one flat colour.
    Flat,
}

/// A state an object is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneState {
    /// Fine.
    Ok,
    /// Needs a look.
    Warning,
    /// Broken or at risk.
    Critical,
    /// Worth knowing.
    Info,
}

/// Where a scene is seen from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneCamera {
    /// A view the home frames to fit the scene.
    #[serde(default)]
    pub preset: CameraPreset,
    /// Where the camera is, in place of the preset's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Xyz>,
    /// What it looks at, in place of the scene's centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Xyz>,
    /// Its vertical field of view, 15 to 90 degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov: Option<Number>,
    /// The person can't turn or zoom it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
    /// It turns slowly about the scene, until the person turns it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spin: bool,
}

/// A camera view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraPreset {
    /// From above and to the side, framing everything.
    #[default]
    Fit,
    /// From the front.
    Front,
    /// From above.
    Top,
    /// From the side.
    Side,
    /// Isometric.
    Iso,
}

/// A lighting preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneLighting {
    /// A key light, a fill and a soft sky.
    #[default]
    Studio,
    /// Soft from every side.
    Soft,
    /// Even, no shading.
    Flat,
}

/// A light added to the preset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneLight {
    /// What kind.
    pub kind: LightKind,
    /// Where it is; a directional light shines from there towards the centre.
    pub at: Xyz,
    /// Its colour; white when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<SceneColor>,
    /// How bright, 0 to 2.
    pub intensity: Number,
}

/// A kind of light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LightKind {
    /// Shines every way from a point.
    Point,
    /// A cone from a point towards the centre.
    Spot,
    /// Parallel rays, like the sun.
    Directional,
}

/// A scene's axes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneAxes {
    /// The x axis's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    /// The y axis's name: up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<String>,
    /// The z axis's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<String>,
    /// Labelled points along them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ticks: Vec<SceneTick>,
    /// A grid on the ground.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub floor: bool,
}

/// A labelled point on an axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneTick {
    /// Which axis.
    pub axis: Axis,
    /// Where along it.
    pub at: Number,
    /// What it reads.
    pub text: String,
}

/// An axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    /// Across.
    X,
    /// Up.
    #[default]
    Y,
    /// Towards the viewer.
    Z,
}

/// An object that moves. Under reduced motion nothing moves; the person can pause any motion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneMotion {
    /// The object.
    pub target: String,
    /// How it moves.
    pub kind: MotionKind,
    /// The axis it turns or moves along.
    #[serde(default)]
    pub axis: Axis,
    /// How long one turn, swing or trip takes, 0.2 to 120.
    pub seconds: Number,
    /// How far it swings or how much it grows: 0 to 100 for `bob`, 0 to 1 for `pulse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Number>,
    /// Seconds before it starts, 0 to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<Number>,
    /// How many times, 1 to 100; it goes on when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<u8>,
    /// The points a `path` motion follows, `x, y, z` flat, in its parent's space.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<Number>,
}

/// How an object moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MotionKind {
    /// Turns about its axis.
    Spin,
    /// Swings up and down along its axis.
    Bob,
    /// Grows and shrinks.
    Pulse,
    /// Circles its parent's centre about its axis.
    Orbit,
    /// Follows its path.
    Path,
}

/// Picking an object fills a choice field the drawing shows, with the object's `value`
/// (spec 3.3 §7.4 rule 5: a scene's pick fills a shown choice field; it never sends). The
/// field stays the way to choose by keyboard and screen reader, and a form's Send still asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneSelect {
    /// The data model path of the choice field it fills, absolute.
    pub path: String,
    /// What picking does, in words, written by the server: "Picking one fills Dock. Nothing
    /// is sent."
    #[serde(default)]
    pub note: String,
}

/// A held component shown beside an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScenePanel {
    /// The object it is shown beside.
    pub anchor: String,
    /// The key of the node's child it shows.
    pub key: String,
}

/// Components a home view holds, presented with CSS 3D, as `HomeNode::Depth`.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{DepthEffect, DrawnDepth};
///
/// let flip = DrawnDepth { effect: DepthEffect::Flip, titles: Vec::new() };
/// assert_eq!(serde_json::to_value(&flip).unwrap()["effect"], "flip");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnDepth {
    /// How the children are presented.
    pub effect: DepthEffect,
    /// A title for each child, for a carousel, wall or stage's controls and the terminal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
}

/// A depth effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DepthEffect {
    /// One child that tilts towards the pointer.
    Tilt,
    /// Two children, front and back; the home's own button turns it over.
    Flip,
    /// Children stacked in depth, the first in front.
    Layers,
    /// Children on a turning ring, one in front at a time.
    Carousel,
    /// Children on a curved wall.
    Wall,
    /// Children on a stage the person can turn.
    Stage,
}

impl DepthEffect {
    /// Whether its children may hold only what shows: a wall or a stage moves them under the
    /// pointer, so they hold no button or field.
    pub fn shows_only(self) -> bool {
        matches!(self, Self::Wall | Self::Stage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_scene_round_trips_with_its_defaults() {
        let wire = json!({
            "kind": "bars",
            "objects": [{
                "id": "bars",
                "shape": { "type": "bars", "at": [0, 0, 1, 0], "heights": [3, 5], "width": 0.6,
                           "series": [0, 1] },
                "look": { "color": "chart2" }
            }],
            "motion": [{ "target": "bars", "kind": "pulse", "seconds": 2 }]
        });
        let scene: DrawnScene = serde_json::from_value(wire).unwrap();
        assert_eq!(scene.camera.preset, CameraPreset::Fit);
        assert_eq!(scene.lighting, SceneLighting::Studio);
        assert_eq!(scene.objects[0].at, origin());
        assert_eq!(scene.objects[0].look.color, SceneColor::Chart2);
        let again = serde_json::to_value(&scene).unwrap();
        assert_eq!(again["objects"][0]["shape"]["heights"], json!([3, 5]));
        assert_eq!(again["motion"][0]["axis"], "y");
    }

    #[test]
    fn shape_fields_are_camel_case() {
        let cone = SceneShape::Cylinder {
            radius_top: 0.into(),
            radius_bottom: 1.into(),
            height: 2.into(),
        };
        let wire = serde_json::to_value(&cone).unwrap();
        assert_eq!(
            wire,
            json!({ "type": "cylinder", "radiusTop": 0, "radiusBottom": 1, "height": 2 })
        );
    }
}
