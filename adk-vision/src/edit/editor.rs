//! [`DeterministicEditor`]: the built-in local `Editor`.

use async_trait::async_trait;

use super::{EditOp, apply_edit};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::Result;
use crate::geometry::CoordFrame;
use crate::media::DecodedImage;
use crate::sent::SentImage;
use crate::traits::Editor;

/// Crops, resizes, redacts, blurs, pixelates and annotates on the `image` crate, in
/// process. Refuses generative edits.
///
/// # Example
///
/// ```
/// use adk_vision::{DeterministicEditor, Editor, Locality};
///
/// let editor = DeterministicEditor::new();
/// assert_eq!(editor.descriptor().locality, Locality::InProcess);
/// ```
#[derive(Clone, Debug)]
pub struct DeterministicEditor {
    descriptor: BackendDescriptor,
}

impl Default for DeterministicEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl DeterministicEditor {
    /// The editor, as backend `deterministic-editor`.
    pub fn new() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "deterministic-editor",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
        }
    }
}

#[async_trait]
impl Editor for DeterministicEditor {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn edit(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        operation: &EditOp,
    ) -> Result<DecodedImage> {
        cx.check()?;
        apply_edit(image.pixels(), image.chain(), operation)
    }
}
