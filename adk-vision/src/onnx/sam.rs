//! Prompt grouping and mask selection shared by the SAM-family segmenters.

use crate::error::{Result, VisionError};
use crate::geometry::BBox;
use crate::traits::SegmentPrompt;

/// One object to segment: an optional box and the points that refine it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PromptGroup {
    /// `(point, positive)` in the backend's frame.
    pub points: Vec<([f32; 2], bool)>,
    /// The box, in the backend's frame.
    pub bbox: Option<BBox>,
}

/// Groups prompts into objects: each box is one object, refined by the points inside it
/// (the first box containing a point takes it); the points outside every box together
/// describe one more object.
///
/// # Errors
///
/// [`VisionError::Unsupported`] for concept and mask prompts, which SAM-family point and box
/// models do not take; [`VisionError::InvalidInput`] for no prompt, or for points outside
/// every box that are all negative.
pub(crate) fn group_prompts(backend: &str, prompts: &[SegmentPrompt]) -> Result<Vec<PromptGroup>> {
    let mut groups: Vec<PromptGroup> = Vec::new();
    let mut points = Vec::new();
    for prompt in prompts {
        match prompt {
            SegmentPrompt::Box { bbox } => {
                groups.push(PromptGroup { points: Vec::new(), bbox: Some(*bbox) });
            }
            SegmentPrompt::Point { at, positive } => points.push((*at, *positive)),
            SegmentPrompt::Concept { text } => {
                return Err(VisionError::Unsupported(format!(
                    "`{backend}` segments from point and box prompts, not the concept `{text}`; \
                     route concept prompts to a concept segmenter such as `sam-3.1`"
                )));
            }
            SegmentPrompt::Mask { .. } => {
                return Err(VisionError::Unsupported(format!(
                    "`{backend}` takes point and box prompts, not mask prompts"
                )));
            }
        }
    }
    let mut loose = Vec::new();
    for (at, positive) in points {
        match groups.iter_mut().find(|g| g.bbox.is_some_and(|b| b.contains(at))) {
            Some(group) => group.points.push((at, positive)),
            None => loose.push((at, positive)),
        }
    }
    if !loose.is_empty() {
        if !loose.iter().any(|(_, positive)| *positive) {
            return Err(VisionError::InvalidInput(format!(
                "`{backend}`: negative points outside every box select nothing; add a positive point or a box"
            )));
        }
        groups.push(PromptGroup { points: loose, bbox: None });
    }
    if groups.is_empty() {
        return Err(VisionError::InvalidInput(format!(
            "`{backend}` needs at least one point or box prompt"
        )));
    }
    Ok(groups)
}

/// The index of the highest predicted IoU, the mask SAM's multimask output recommends.
pub(crate) fn best_mask(iou: &[f32]) -> Option<usize> {
    iou.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_take_the_points_inside_them_and_loose_points_form_one_more_object() {
        let prompts = [
            SegmentPrompt::Box { bbox: BBox::new(0.0, 0.0, 10.0, 10.0) },
            SegmentPrompt::Point { at: [5.0, 5.0], positive: false },
            SegmentPrompt::Point { at: [50.0, 50.0], positive: true },
            SegmentPrompt::Point { at: [60.0, 60.0], positive: false },
        ];
        let groups = group_prompts("sam", &prompts).unwrap();
        assert_eq!(
            groups,
            vec![
                PromptGroup {
                    points: vec![([5.0, 5.0], false)],
                    bbox: Some(BBox::new(0.0, 0.0, 10.0, 10.0)),
                },
                PromptGroup {
                    points: vec![([50.0, 50.0], true), ([60.0, 60.0], false)],
                    bbox: None,
                },
            ]
        );
    }

    #[test]
    fn concepts_masks_nothing_and_lone_negatives_are_refused() {
        let concept = [SegmentPrompt::Concept { text: "cat".into() }];
        assert!(matches!(group_prompts("sam", &concept), Err(VisionError::Unsupported(_))));
        assert!(matches!(group_prompts("sam", &[]), Err(VisionError::InvalidInput(_))));
        let negative = [SegmentPrompt::Point { at: [1.0, 1.0], positive: false }];
        assert!(matches!(group_prompts("sam", &negative), Err(VisionError::InvalidInput(_))));
    }

    #[test]
    fn the_best_mask_has_the_highest_predicted_iou() {
        assert_eq!(best_mask(&[0.2, 0.9, 0.5]), Some(1));
        assert_eq!(best_mask(&[]), None);
    }
}
