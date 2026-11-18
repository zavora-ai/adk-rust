//! Rule 5 for 3D scenes and depth effects (spec 3.3 §6.1 `genui`, extension catalogs): every
//! scene within its limits, every string it shows checked, its summary written here, and a
//! pick that fills only a choice field the drawing shows.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Number, Value};

use super::{
    Check, Child, Choice, Component, DrawRefusal, ExtensionView, HomeSeen, INPUTS, MAX_DEPTH,
    MAX_DEPTH_CHILDREN, MAX_SCENE_BYTES, MAX_SCENE_GRID_SIDE, MAX_SCENE_GROUP_DEPTH,
    MAX_SCENE_INSTANCES, MAX_SCENE_LIGHTS, MAX_SCENE_LINE_POINTS, MAX_SCENE_MOTIONS,
    MAX_SCENE_OBJECTS, MAX_SCENE_PANELS, MAX_SCENE_TEXT_CHARS, MAX_SCENE_TEXTS, Place, Select,
    SurfaceDraft, Walk, children,
};
use crate::genui::catalog::{HomeNode, HomePanel, HomeScene};
use crate::genui::drawn::DrawnKind;
use crate::genui::scene::{
    DepthEffect, DrawnDepth, DrawnScene, MotionKind, SceneKind, SceneObject, SceneShape, Xyz,
};
use crate::words;

/// The furthest anything in a scene is from its origin, and the largest size.
const MAX_EXTENT: f64 = 10_000.0;
/// The most labelled ticks a scene's axes have.
const MAX_SCENE_TICKS: usize = 48;
/// The most points a `path` motion follows.
const MAX_PATH_POINTS: usize = 64;
/// The longest option value an object picks.
const MAX_VALUE_CHARS: usize = 200;
/// How many of a scene's labels its summary names.
const SUMMARY_NAMES: usize = 3;

fn float(number: &Number) -> f64 {
    number.as_f64().unwrap_or(f64::NAN)
}

/// What a scene's checks have counted.
#[derive(Default)]
struct Counts {
    texts: usize,
    instances: usize,
    line_points: usize,
    /// Bars and points, by kind, for the summary.
    bars: usize,
    points: usize,
    links: usize,
    cells: usize,
    /// Every bar height and grid height, for the summary's range.
    low: f64,
    high: f64,
}

impl Counts {
    fn new() -> Self {
        Self { low: f64::INFINITY, high: f64::NEG_INFINITY, ..Self::default() }
    }

    fn value(&mut self, value: f64) {
        self.low = self.low.min(value);
        self.high = self.high.max(value);
    }

    fn range(&self, values: Option<&crate::genui::scene::SceneValues>) -> Option<(f64, f64)> {
        let map = |height: f64| values.map_or(height, |values| values.value(height));
        (self.low <= self.high).then_some((map(self.low), map(self.high)))
    }
}

impl Check {
    /// A scene within its limits, every string it shows checked, its panels and text view
    /// checked where they are placed, and the fields only the server writes written here.
    pub(super) fn scene(
        &self,
        at: (&str, &str),
        item: Option<&str>,
        home: HomeScene,
        depth: usize,
        seen: &mut HomeSeen,
    ) -> Result<HomeScene, DrawRefusal> {
        let HomeScene { mut scene, panels, text } = home;
        let refuse = |detail: String| self.cannot_show(Some(at), format!("its scene {detail}"));
        if scene.objects.is_empty() {
            return Err(refuse("has no objects".into()));
        }
        if scene.objects.len() > MAX_SCENE_OBJECTS {
            return Err(refuse(format!(
                "has {} objects; at most {MAX_SCENE_OBJECTS}",
                scene.objects.len()
            )));
        }
        if let Some(height) = scene.height
            && !(200..=960).contains(&height)
        {
            return Err(refuse(format!("is {height} pixels tall; 200 to 960")));
        }
        if let Some(values) = &scene.values {
            let (scale, offset) = (float(&values.scale), float(&values.offset));
            if !(scale > 0.0 && scale.is_finite() && offset.is_finite()) {
                return Err(refuse("maps its heights with a scale that is not above 0".into()));
            }
        }
        let mut counts = Counts::new();
        let ids = self.scene_ids(&scene, &refuse)?;
        for object in &scene.objects {
            self.scene_object(at, item, object, &mut counts, &refuse)?;
        }
        if counts.instances > MAX_SCENE_INSTANCES {
            return Err(refuse(format!(
                "draws {} bars and points; at most {MAX_SCENE_INSTANCES}",
                counts.instances
            )));
        }
        if counts.line_points > MAX_SCENE_LINE_POINTS {
            return Err(refuse(format!(
                "has {} points on its lines; at most {MAX_SCENE_LINE_POINTS}",
                counts.line_points
            )));
        }
        self.scene_view(at, item, &scene, &ids, &mut counts, &refuse)?;
        if counts.texts > MAX_SCENE_TEXTS {
            return Err(refuse(format!(
                "shows {} strings; at most {MAX_SCENE_TEXTS}",
                counts.texts
            )));
        }

        let anchors: BTreeSet<String> = ids.into_keys().map(str::to_string).collect();

        // Picking: every object with a value is labelled, and some field is filled.
        let values: Vec<String> = scene
            .objects
            .iter()
            .filter_map(|object| object.value.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        match &mut scene.select {
            Some(select) => {
                if !select.path.starts_with('/') || select.path.chars().any(char::is_whitespace) {
                    return Err(refuse(format!(
                        "fills '{}'; a pick fills an absolute data model path",
                        select.path
                    )));
                }
                if values.is_empty() {
                    return Err(refuse("fills a field, but no object has a value to pick".into()));
                }
                select.note = String::new();
                seen.selects.push(Select {
                    id: at.0.to_string(),
                    component: at.1.to_string(),
                    path: select.path.clone(),
                    values,
                });
            }
            None if !values.is_empty() => {
                return Err(refuse("has objects with values, but picking fills no field".into()));
            }
            None => {}
        }

        // Panels show beside objects; the text view says it in words.
        if panels.len() > MAX_SCENE_PANELS {
            return Err(refuse(format!("has {} panels; at most {MAX_SCENE_PANELS}", panels.len())));
        }
        let mut checked = Vec::with_capacity(panels.len());
        for HomePanel { anchor, content } in panels {
            if !anchors.contains(&anchor) {
                return Err(refuse(format!(
                    "shows a panel beside '{anchor}', which it doesn't have"
                )));
            }
            let content = self.home(at, item, content, depth + 1, seen, Place::Panel)?;
            checked.push(HomePanel { anchor, content });
        }
        let text = text
            .map(|text| self.home(at, item, text, depth + 1, seen, Place::TextView))
            .transpose()?;

        scene.panels = Vec::new();
        scene.text_view = None;
        scene.summary = summary(&scene, &counts);
        self.shows(at, item, "scene/summary", &scene.summary)?;
        let bytes = serde_json::to_vec(&scene).map_or(usize::MAX, |bytes| bytes.len());
        if bytes > MAX_SCENE_BYTES {
            return Err(refuse(format!("takes {bytes} bytes; at most {MAX_SCENE_BYTES}")));
        }
        Ok(HomeScene { scene, panels: checked, text })
    }

    /// Every object's id, unique and valid, with its group depth; parents are groups that
    /// exist, nested at most [`MAX_SCENE_GROUP_DEPTH`] deep, without loops.
    fn scene_ids<'s>(
        &self,
        scene: &'s DrawnScene,
        refuse: &impl Fn(String) -> DrawRefusal,
    ) -> Result<BTreeMap<&'s str, &'s SceneObject>, DrawRefusal> {
        let mut ids = BTreeMap::new();
        for object in &scene.objects {
            if !super::is_identifier(&object.id) || ids.insert(object.id.as_str(), object).is_some()
            {
                return Err(refuse(format!(
                    "has an object id '{}' that is not unique or valid",
                    super::truncate(&object.id)
                )));
            }
        }
        for object in &scene.objects {
            let mut parent = object.parent.as_deref();
            let mut levels = 0;
            while let Some(id) = parent {
                let Some(group) = ids.get(id) else {
                    return Err(refuse(format!(
                        "puts '{}' in '{id}', which it doesn't have",
                        object.id
                    )));
                };
                if group.shape != SceneShape::Group {
                    return Err(refuse(format!(
                        "puts '{}' in '{id}', which is not a group",
                        object.id
                    )));
                }
                levels += 1;
                if levels > MAX_SCENE_GROUP_DEPTH {
                    return Err(refuse(format!(
                        "nests groups deeper than {MAX_SCENE_GROUP_DEPTH}, or in a loop"
                    )));
                }
                parent = group.parent.as_deref();
            }
        }
        Ok(ids)
    }

    /// One object: its place, its look, its words and its shape.
    fn scene_object(
        &self,
        at: (&str, &str),
        item: Option<&str>,
        object: &SceneObject,
        counts: &mut Counts,
        refuse: &impl Fn(String) -> DrawRefusal,
    ) -> Result<(), DrawRefusal> {
        let id = object.id.as_str();
        point(&object.at, MAX_EXTENT).map_err(|why| refuse(format!("places '{id}' {why}")))?;
        if let Some(turn) = &object.turn {
            point(turn, MAX_EXTENT).map_err(|why| refuse(format!("turns '{id}' {why}")))?;
        }
        if let Some(scale) = &object.scale
            && scale.iter().map(float).any(|value| !(0.001..=1_000.0).contains(&value))
        {
            return Err(refuse(format!("scales '{id}' outside 0.001 to 1000")));
        }
        if let Some(opacity) = &object.look.opacity
            && !(0.15..=1.0).contains(&float(opacity))
        {
            return Err(refuse(format!("draws '{id}' at an opacity outside 0.15 to 1")));
        }
        if let Some(label) = &object.label {
            self.scene_text(at, item, &format!("scene/objects/{id}/label"), label, counts, refuse)?;
        }
        if object.state.is_some() && object.label.is_none() {
            return Err(refuse(format!(
                "gives '{id}' a state but no label; a state is a word as well as a colour"
            )));
        }
        if let Some(value) = &object.value {
            if object.label.is_none() {
                return Err(refuse(format!(
                    "lets '{id}' be picked without a label; the person sees what they pick"
                )));
            }
            if value.is_empty() || value.chars().count() > MAX_VALUE_CHARS {
                return Err(refuse(format!(
                    "gives '{id}' a value that is empty or longer than {MAX_VALUE_CHARS} characters"
                )));
            }
        }
        let size = |value: &Number, what: &str| -> Result<f64, DrawRefusal> {
            let value = float(value);
            if value > 0.0 && value <= MAX_EXTENT {
                Ok(value)
            } else {
                Err(refuse(format!("gives '{id}' a {what} outside 0 to {MAX_EXTENT}")))
            }
        };
        match &object.shape {
            SceneShape::Group => {}
            SceneShape::Box { size: sides } => {
                for side in sides {
                    size(side, "size")?;
                }
            }
            SceneShape::Sphere { radius } | SceneShape::Globe { radius } => {
                size(radius, "radius")?;
            }
            SceneShape::Cylinder { radius_top, radius_bottom, height } => {
                let (top, bottom) = (float(radius_top), float(radius_bottom));
                if !(0.0..=MAX_EXTENT).contains(&top)
                    || !(0.0..=MAX_EXTENT).contains(&bottom)
                    || top + bottom <= 0.0
                {
                    return Err(refuse(format!("gives '{id}' radii outside 0 to {MAX_EXTENT}")));
                }
                size(height, "height")?;
            }
            SceneShape::Torus { radius, tube } => {
                let radius = size(radius, "radius")?;
                if size(tube, "tube")? > radius {
                    return Err(refuse(format!("gives '{id}' a tube wider than its ring")));
                }
            }
            SceneShape::Plane { width, depth } => {
                size(width, "width")?;
                size(depth, "depth")?;
            }
            SceneShape::Line { points, segments, .. } => {
                let count = flat(points, 3).map_err(|why| refuse(format!("draws '{id}' {why}")))?;
                if count < 2 || (*segments && !count.is_multiple_of(2)) {
                    return Err(refuse(format!(
                        "draws '{id}' through {count} points; a line has 2 or more, segments a pair each"
                    )));
                }
                counts.line_points += count;
                counts.links += if *segments { count / 2 } else { count - 1 };
            }
            SceneShape::Bars { at: places, heights, width, series } => {
                let count = flat(places, 2).map_err(|why| refuse(format!("draws '{id}' {why}")))?;
                if heights.len() != count || (!series.is_empty() && series.len() != count) {
                    return Err(refuse(format!(
                        "draws '{id}' with {} places, {} heights and {} series",
                        count,
                        heights.len(),
                        series.len()
                    )));
                }
                size(width, "width")?;
                for height in heights {
                    let height = float(height);
                    if height.abs() > MAX_EXTENT {
                        return Err(refuse(format!("draws a bar of '{id}' beyond {MAX_EXTENT}")));
                    }
                    counts.value(height);
                }
                series_in_range(series).map_err(|why| refuse(format!("draws '{id}' {why}")))?;
                counts.instances += count;
                counts.bars += count;
            }
            SceneShape::Points { at: places, size: radius, series } => {
                let count = flat(places, 3).map_err(|why| refuse(format!("draws '{id}' {why}")))?;
                if !series.is_empty() && series.len() != count {
                    return Err(refuse(format!(
                        "draws '{id}' with {count} points and {} series",
                        series.len()
                    )));
                }
                size(radius, "point size")?;
                series_in_range(series).map_err(|why| refuse(format!("draws '{id}' {why}")))?;
                counts.instances += count;
                counts.points += count;
            }
            SceneShape::Grid { columns, rows, heights, width, depth } => {
                let side = 2..=MAX_SCENE_GRID_SIDE;
                if !side.contains(columns) || !side.contains(rows) {
                    return Err(refuse(format!(
                        "draws '{id}' as a {columns} by {rows} grid; each side 2 to {MAX_SCENE_GRID_SIDE}"
                    )));
                }
                let cells = usize::from(*columns) * usize::from(*rows);
                if heights.len() != cells {
                    return Err(refuse(format!(
                        "draws '{id}' with {} heights for {cells} points",
                        heights.len()
                    )));
                }
                for height in heights {
                    let height = float(height);
                    if height.abs() > MAX_EXTENT {
                        return Err(refuse(format!("draws '{id}' beyond {MAX_EXTENT}")));
                    }
                    counts.value(height);
                }
                size(width, "width")?;
                size(depth, "depth")?;
                counts.cells += cells;
            }
            SceneShape::Label { text, .. } => {
                if text.trim().is_empty() {
                    return Err(refuse(format!("shows '{id}' as an empty label")));
                }
                self.scene_text(
                    at,
                    item,
                    &format!("scene/objects/{id}/text"),
                    text,
                    counts,
                    refuse,
                )?;
            }
        }
        Ok(())
    }

    /// The camera, the lights, the axes and the motion.
    fn scene_view(
        &self,
        at: (&str, &str),
        item: Option<&str>,
        scene: &DrawnScene,
        ids: &BTreeMap<&str, &SceneObject>,
        counts: &mut Counts,
        refuse: &impl Fn(String) -> DrawRefusal,
    ) -> Result<(), DrawRefusal> {
        for place in [&scene.camera.at, &scene.camera.target].into_iter().flatten() {
            point(place, MAX_EXTENT).map_err(|why| refuse(format!("places its camera {why}")))?;
        }
        if let Some(fov) = &scene.camera.fov
            && !(15.0..=90.0).contains(&float(fov))
        {
            return Err(refuse("has a field of view outside 15 to 90 degrees".into()));
        }
        if scene.lights.len() > MAX_SCENE_LIGHTS {
            return Err(refuse(format!(
                "adds {} lights; at most {MAX_SCENE_LIGHTS}",
                scene.lights.len()
            )));
        }
        for light in &scene.lights {
            point(&light.at, MAX_EXTENT).map_err(|why| refuse(format!("places a light {why}")))?;
            if !(0.0..=2.0).contains(&float(&light.intensity)) {
                return Err(refuse("has a light brighter than 2 or below 0".into()));
            }
        }
        if let Some(axes) = &scene.axes {
            if axes.ticks.len() > MAX_SCENE_TICKS {
                return Err(refuse(format!("has more than {MAX_SCENE_TICKS} ticks")));
            }
            for (name, axis) in [("x", &axes.x), ("y", &axes.y), ("z", &axes.z)] {
                if let Some(axis) = axis {
                    self.scene_text(at, item, &format!("scene/axes/{name}"), axis, counts, refuse)?;
                }
            }
            for tick in &axes.ticks {
                if float(&tick.at).abs() > MAX_EXTENT {
                    return Err(refuse(format!("has a tick beyond {MAX_EXTENT}")));
                }
                self.scene_text(at, item, "scene/axes/tick", &tick.text, counts, refuse)?;
            }
        }
        if scene.motion.len() > MAX_SCENE_MOTIONS {
            return Err(refuse(format!(
                "moves {} objects; at most {MAX_SCENE_MOTIONS}",
                scene.motion.len()
            )));
        }
        for motion in &scene.motion {
            let target = motion.target.as_str();
            if !ids.contains_key(target) {
                return Err(refuse(format!("moves '{target}', which it doesn't have")));
            }
            if !(0.2..=120.0).contains(&float(&motion.seconds)) {
                return Err(refuse(format!(
                    "moves '{target}' over a time outside 0.2 to 120 seconds"
                )));
            }
            let most = if motion.kind == MotionKind::Pulse { 1.0 } else { 100.0 };
            if let Some(amount) = &motion.amount
                && !(0.0..=most).contains(&float(amount))
            {
                return Err(refuse(format!("moves '{target}' by an amount outside 0 to {most}")));
            }
            if let Some(delay) = &motion.delay
                && !(0.0..=30.0).contains(&float(delay))
            {
                return Err(refuse(format!("delays '{target}' outside 0 to 30 seconds")));
            }
            if motion.repeat.is_some_and(|repeat| !(1..=100).contains(&repeat)) {
                return Err(refuse(format!("repeats '{target}' outside 1 to 100 times")));
            }
            match motion.kind {
                MotionKind::Path => {
                    let points = flat(&motion.path, 3)
                        .map_err(|why| refuse(format!("moves '{target}' along a path {why}")))?;
                    if !(2..=MAX_PATH_POINTS).contains(&points) {
                        return Err(refuse(format!(
                            "moves '{target}' along {points} points; 2 to {MAX_PATH_POINTS}"
                        )));
                    }
                }
                _ if !motion.path.is_empty() => {
                    return Err(refuse(format!("gives '{target}' a path it doesn't follow")));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A string a scene shows: short, and checked like every other.
    fn scene_text(
        &self,
        at: (&str, &str),
        item: Option<&str>,
        field: &str,
        text: &str,
        counts: &mut Counts,
        refuse: &impl Fn(String) -> DrawRefusal,
    ) -> Result<(), DrawRefusal> {
        if text.chars().count() > MAX_SCENE_TEXT_CHARS {
            return Err(refuse(format!(
                "shows '{}', longer than {MAX_SCENE_TEXT_CHARS} characters",
                super::truncate(text)
            )));
        }
        counts.texts += 1;
        self.shows(at, item, field, text)?;
        Ok(())
    }

    /// A depth effect within its limits: the children it takes, and titles that are checked.
    pub(super) fn depth(
        &self,
        at: (&str, &str),
        item: Option<&str>,
        depth: DrawnDepth,
        children: usize,
    ) -> Result<DrawnDepth, DrawRefusal> {
        let refuse =
            |detail: String| self.cannot_show(Some(at), format!("its depth effect {detail}"));
        let fits = match depth.effect {
            DepthEffect::Tilt => children == 1,
            DepthEffect::Flip => children == 2,
            _ => (1..=MAX_DEPTH_CHILDREN).contains(&children),
        };
        if !fits {
            return Err(refuse(format!(
                "presents {children} children; tilt takes 1, flip 2 and the others 1 to {MAX_DEPTH_CHILDREN}"
            )));
        }
        if !depth.titles.is_empty() && depth.titles.len() != children {
            return Err(refuse(format!(
                "has {} titles for {children} children",
                depth.titles.len()
            )));
        }
        for title in &depth.titles {
            if title.chars().count() > MAX_SCENE_TEXT_CHARS {
                return Err(refuse(format!(
                    "has a title longer than {MAX_SCENE_TEXT_CHARS} characters"
                )));
            }
            self.shows(at, item, "depth/title", title)?;
        }
        Ok(depth)
    }

    /// Every scene's pick fills a choice field the drawing shows, with values among its
    /// options; then each scene says so in words.
    pub(super) fn selects(
        &self,
        draft: &SurfaceDraft,
        selects: &[Select],
        choices: &BTreeMap<String, Choice>,
        extensions: &mut BTreeMap<(String, String), ExtensionView>,
    ) -> Result<(), DrawRefusal> {
        if selects.is_empty() {
            return Ok(());
        }
        for select in selects {
            let at = Some((select.id.as_str(), select.component.as_str()));
            if matches!(draft.kind, DrawnKind::Card | DrawnKind::Table) {
                return Err(
                    self.cannot_show(at, "a card or table has no fields for a scene's pick")
                );
            }
            let Some(choice) = choices.get(&select.path) else {
                return Err(self.cannot_show(
                    at,
                    format!(
                        "its scene's pick fills {}, which no choice field the drawing shows holds",
                        select.path
                    ),
                ));
            };
            if let Some(value) = select.values.iter().find(|value| !choice.options.contains(value))
            {
                return Err(self.cannot_show(
                    at,
                    format!(
                        "its scene picks '{}', which is not one of {}'s options",
                        super::truncate(value),
                        select.path
                    ),
                ));
            }
        }
        for view in extensions.values_mut() {
            self.notes(&mut view.home, choices)?;
        }
        Ok(())
    }

    /// Writes what each scene's pick fills, in words, into `node`.
    fn notes(
        &self,
        node: &mut HomeNode,
        choices: &BTreeMap<String, Choice>,
    ) -> Result<(), DrawRefusal> {
        match node {
            HomeNode::Scene(home) => {
                if let Some(select) = &mut home.scene.select
                    && let Some(choice) = choices.get(&select.path)
                {
                    let field =
                        if choice.label.trim().is_empty() { "the choice" } else { &choice.label };
                    select.note = words::scene_picks(field);
                    self.display(None, None, "scene/select/note", &select.note)?;
                }
            }
            HomeNode::Row(children) | HomeNode::Column(children) | HomeNode::Card(children) => {
                for child in children {
                    self.notes(child, choices)?;
                }
            }
            HomeNode::Tabs(tabs) => {
                for (_, child) in tabs {
                    self.notes(child, choices)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl<'a> Walk<'a> {
    /// Records a choice field the drawing shows: its absolute path, label and options.
    pub(super) fn choice(&mut self, scope: &str, component: &Component<'_>, label: Option<String>) {
        let Some(path) = component
            .props
            .get("value")
            .and_then(|value| value.get("path"))
            .and_then(Value::as_str)
        else {
            return;
        };
        let path = if path.starts_with('/') { path.to_string() } else { format!("{scope}/{path}") };
        let options = component
            .props
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|option| option.get("value")?.as_str().map(str::to_string))
            .collect();
        self.choices.insert(path, Choice { label: label.unwrap_or_default(), options });
    }

    /// The first button or field in the subtree of component `id`, if any.
    pub(super) fn interactive(&self, id: &str) -> Option<(String, String)> {
        fn walk(
            components: &BTreeMap<&str, Component<'_>>,
            catalog: &crate::genui::catalog::Catalog,
            id: &str,
            depth: usize,
            seen: &mut BTreeSet<String>,
        ) -> Option<(String, String)> {
            if depth > MAX_DEPTH || !seen.insert(id.to_string()) {
                return None;
            }
            let component = components.get(id)?;
            if component.name == "Button" || INPUTS.contains(&component.name) {
                return Some((id.to_string(), component.name.to_string()));
            }
            children(catalog, component.name, component.props).into_iter().find_map(|child| {
                let child = match child {
                    Child::Id(child) | Child::Template { component: child, .. } => child,
                };
                walk(components, catalog, child, depth + 1, seen)
            })
        }
        walk(self.components, self.check.catalog, id, 0, &mut BTreeSet::new())
    }
}

/// `place` within `extent` of the origin on every axis.
fn point(place: &Xyz, extent: f64) -> Result<(), String> {
    if place.iter().map(float).all(|value| value.abs() <= extent) {
        Ok(())
    } else {
        Err(format!("beyond {extent} from the origin"))
    }
}

/// How many points a flat list of `per`-number points holds, each within reach.
fn flat(numbers: &[Number], per: usize) -> Result<usize, String> {
    if !numbers.len().is_multiple_of(per) {
        return Err(format!("with {} numbers, not {per} per point", numbers.len()));
    }
    if numbers.iter().map(float).any(|value| value.abs() > MAX_EXTENT) {
        return Err(format!("beyond {MAX_EXTENT} from the origin"));
    }
    Ok(numbers.len() / per)
}

fn series_in_range(series: &[u8]) -> Result<(), String> {
    if series.iter().any(|index| *index > 7) {
        Err("with a series beyond the eighth chart colour".into())
    } else {
        Ok(())
    }
}

/// The scene's summary, by its kind: what it shows and how much.
fn summary(scene: &DrawnScene, counts: &Counts) -> String {
    let axis = |name: &Option<String>| name.clone();
    let axes = scene.axes.clone().unwrap_or_default();
    match scene.kind {
        SceneKind::Bars => {
            let named: Vec<String> = axis(&axes.y).into_iter().collect();
            let named: Vec<&str> = named.iter().map(String::as_str).collect();
            words::scene_summary(
                "3D bar chart",
                &named,
                &words::counted(counts.bars, "bar", "bars"),
                counts.range(scene.values.as_ref()),
            )
        }
        SceneKind::Points => {
            let named: Vec<String> =
                [&axes.x, &axes.y, &axes.z].into_iter().flatten().cloned().collect();
            let named: Vec<&str> = named.iter().map(String::as_str).collect();
            words::scene_summary(
                "3D scatter chart",
                &named,
                &words::counted(counts.points, "point", "points"),
                None,
            )
        }
        SceneKind::Surface => {
            let named: Vec<String> = axis(&axes.y).into_iter().collect();
            let named: Vec<&str> = named.iter().map(String::as_str).collect();
            words::scene_summary(
                "3D surface",
                &named,
                &words::counted(counts.cells, "value", "values"),
                counts.range(scene.values.as_ref()),
            )
        }
        SceneKind::Network => words::scene_summary(
            "3D network",
            &[],
            &format!(
                "{} and {}",
                words::counted(counts.points, "node", "nodes"),
                words::counted(counts.links, "link", "links")
            ),
            None,
        ),
        SceneKind::Globe => {
            let routes = scene
                .objects
                .iter()
                .filter(|object| matches!(object.shape, SceneShape::Line { .. }))
                .count();
            words::scene_summary(
                "Globe",
                &[],
                &format!(
                    "{} and {}",
                    words::counted(counts.points, "place", "places"),
                    words::counted(routes, "route", "routes")
                ),
                None,
            )
        }
        SceneKind::Space => {
            let labels: Vec<&str> =
                scene.objects.iter().filter_map(|object| object.label.as_deref()).collect();
            let mut named: Vec<String> =
                labels.iter().take(SUMMARY_NAMES).map(|label| (*label).to_string()).collect();
            if labels.len() > SUMMARY_NAMES {
                named.push(format!("{} more", labels.len() - SUMMARY_NAMES));
            }
            let named: Vec<&str> = named.iter().map(String::as_str).collect();
            let shapes =
                scene.objects.iter().filter(|object| object.shape != SceneShape::Group).count();
            words::scene_summary(
                "3D scene",
                &named,
                &words::counted(shapes, "object", "objects"),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genui::scene::SceneValues;

    #[test]
    fn a_summary_speaks_the_datas_numbers_not_the_scenes_heights() {
        let mut scene = DrawnScene::new(SceneKind::Bars);
        scene.axes =
            Some(crate::genui::SceneAxes { y: Some("Revenue".into()), ..Default::default() });
        let mut counts = Counts::new();
        counts.bars = 2;
        counts.value(2.0);
        counts.value(6.0);
        assert_eq!(summary(&scene, &counts), "3D bar chart of Revenue, 2 bars, from 2 to 6");
        scene.values =
            Some(SceneValues { scale: Number::from_f64(1.05).unwrap(), offset: Number::from(0) });
        assert_eq!(summary(&scene, &counts), "3D bar chart of Revenue, 2 bars, from 2.1 to 6.3");
    }
}
