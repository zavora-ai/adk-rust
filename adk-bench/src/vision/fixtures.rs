//! The committed vision eval sets as bench workloads.

use std::path::{Path, PathBuf};

use adk_eval::TestFile;
use adk_eval::schema::Part;
use adk_vision::{DecodeLimits, DecodedImage, SourceImage};

use super::{VisionWorkload, vision_error};
use crate::error::{BenchError, Result};

/// The vision eval sets: `ADK_VISION_EVAL_DIR` when set, else `adk-vision/tests/fixtures/eval`
/// in the source tree this crate was built from.
pub fn eval_dir() -> PathBuf {
    std::env::var_os("ADK_VISION_EVAL_DIR").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../adk-vision/tests/fixtures/eval")
    })
}

/// The workloads each kind of backend runs, loaded from one eval directory.
pub struct FixtureSet {
    dir: PathBuf,
}

impl FixtureSet {
    /// The eval sets in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Loads `name.test.json`, verifying its fixtures, with the scores to report and the
    /// first case's first turn as the latency input.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Vision`] when the eval set does not load or its first turn
    /// shows no image.
    pub fn workload(&self, name: &str, scores: &[&'static str]) -> Result<VisionWorkload> {
        let path = self.dir.join(format!("{name}.test.json"));
        let eval_set = TestFile::load(&path)
            .map_err(|e| BenchError::Vision(format!("{}: {e}", path.display())))?;
        let turn = eval_set
            .eval_cases
            .first()
            .and_then(|case| case.conversation.first())
            .ok_or_else(|| BenchError::Vision(format!("{} has no turns", path.display())))?;
        let mut image = None;
        let mut text = String::new();
        for part in &turn.user_content.parts {
            match part {
                Part::Image { image: fixture } => {
                    let bytes = fixture.bytes().ok_or_else(|| {
                        BenchError::Vision(format!("{} was not loaded", fixture.path))
                    })?;
                    image = Some(
                        SourceImage::decode(bytes, &DecodeLimits::default())
                            .map_err(vision_error)?,
                    );
                }
                Part::Text { text: t } => text.push_str(t),
                Part::FunctionCall { .. } | Part::FunctionResponse { .. } => {}
            }
        }
        let image = image.ok_or_else(|| {
            BenchError::Vision(format!("{}'s first turn shows no image", path.display()))
        })?;
        Ok(VisionWorkload {
            name: name.to_string(),
            eval_set,
            scores: scores.to_vec(),
            latency: Some((image, text)),
        })
    }

    /// Loads an image fixture relative to the eval directory.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Vision`] when it cannot be read or decoded.
    pub fn image(&self, relative: &str) -> Result<SourceImage> {
        let path = self.dir.join(relative);
        let bytes = std::fs::read(&path)
            .map_err(|e| BenchError::Vision(format!("{}: {e}", path.display())))?;
        SourceImage::decode(&bytes, &DecodeLimits::default()).map_err(vision_error)
    }
}

/// `image` resized so its long edge is `long_edge` pixels, aspect kept (Catmull-Rom).
///
/// # Errors
///
/// Returns [`BenchError::Vision`] when the resized image cannot be built.
pub fn scaled(image: &SourceImage, long_edge: u32) -> Result<SourceImage> {
    let (w, h) = image.size();
    let scale = f64::from(long_edge) / f64::from(w.max(h));
    let (sw, sh) = (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    );
    let buffer = image::RgbaImage::from_raw(w, h, image.pixels().rgba().to_vec())
        .ok_or_else(|| BenchError::Vision("the image's pixel buffer has the wrong size".into()))?;
    let resized = image::imageops::resize(&buffer, sw, sh, image::imageops::FilterType::CatmullRom);
    let pixels = DecodedImage::from_rgba(sw, sh, resized.into_raw()).map_err(vision_error)?;
    SourceImage::from_pixels(pixels).map_err(vision_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_eval_sets_load_as_workloads() {
        let set = FixtureSet::new(eval_dir());
        for (name, size) in [
            ("shapes-segment", (960, 540)),
            ("shapes-concepts", (960, 540)),
            ("shapes-locate", (960, 540)),
            ("s5-grounding", (3024, 1964)),
            ("documents-read", (1000, 1300)),
        ] {
            let workload = set.workload(name, &["region_iou"]).unwrap();
            let (image, _) = workload.latency.as_ref().unwrap();
            assert_eq!(image.size(), size, "{name}");
        }
    }

    #[test]
    fn scaling_keeps_the_aspect() {
        let image = SourceImage::from_pixels(DecodedImage::filled(1000, 1300, [255; 4])).unwrap();
        assert_eq!(scaled(&image, 512).unwrap().size(), (394, 512));
        assert_eq!(scaled(&image, 2048).unwrap().size(), (1575, 2048));
    }
}
