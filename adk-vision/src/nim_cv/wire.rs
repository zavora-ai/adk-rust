//! The CV endpoints' JSON: `{"input": [{"type": "image_url", "url": <data URI>}]}` in, and
//! text detections or class-keyed boxes out, every coordinate in 0–1 of the image sent.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Result, VisionError};

/// A point in 0–1 of the image sent.
#[derive(Clone, Copy, Debug, Deserialize)]
pub(crate) struct NormPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Debug, Deserialize)]
struct Quad {
    points: Vec<NormPoint>,
}

#[derive(Debug, Deserialize)]
struct Prediction {
    text: String,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct Detection {
    text_prediction: Prediction,
    bounding_box: Quad,
}

#[derive(Debug, Deserialize)]
struct OcrItem {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    text_detections: Vec<Detection>,
}

#[derive(Debug, Deserialize)]
struct OcrResponse {
    data: Vec<OcrItem>,
}

/// One recognised piece of text: its quad, clockwise from top-left, in 0–1.
#[derive(Clone, Debug)]
pub(crate) struct TextDetection {
    pub(crate) points: Vec<NormPoint>,
    pub(crate) text: String,
    pub(crate) confidence: Option<f32>,
}

/// A box in 0–1 with the class's confidence.
#[derive(Clone, Copy, Debug, Deserialize)]
pub(crate) struct NormBox {
    pub(crate) x_min: f32,
    pub(crate) y_min: f32,
    pub(crate) x_max: f32,
    pub(crate) y_max: f32,
    #[serde(default)]
    pub(crate) confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct ElementsItem {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    bounding_boxes: BTreeMap<String, Vec<NormBox>>,
}

#[derive(Debug, Deserialize)]
struct ElementsResponse {
    data: Vec<ElementsItem>,
}

/// The request body for one inline image, with OCR merge levels when given.
pub(crate) fn body(data_uri: &str, merge_levels: Option<&[&str]>) -> Value {
    let mut body = json!({ "input": [{ "type": "image_url", "url": data_uri }] });
    if let Some(levels) = merge_levels {
        body["merge_levels"] = json!(levels);
    }
    body
}

fn parse_error(backend: &str, model: &str, message: impl std::fmt::Display) -> VisionError {
    VisionError::Parse { backend: backend.to_string(), message: format!("`{model}`: {message}") }
}

fn finite(backend: &str, model: &str, values: &[f32]) -> Result<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(parse_error(backend, model, format!("a coordinate is not finite: {values:?}")))
    }
}

/// The text detections for the first (only) image of a request.
pub(crate) fn text_detections(
    backend: &str,
    model: &str,
    value: Value,
) -> Result<Vec<TextDetection>> {
    let response: OcrResponse =
        serde_json::from_value(value).map_err(|e| parse_error(backend, model, e))?;
    let Some(item) = response.data.into_iter().find(|item| item.index == 0) else {
        return Err(parse_error(backend, model, "the answer holds no result for the image"));
    };
    item.text_detections
        .into_iter()
        .map(|d| {
            if d.bounding_box.points.len() < 3 {
                return Err(parse_error(
                    backend,
                    model,
                    format!("a text box has {} points", d.bounding_box.points.len()),
                ));
            }
            let coords: Vec<f32> = d.bounding_box.points.iter().flat_map(|p| [p.x, p.y]).collect();
            finite(backend, model, &coords)?;
            Ok(TextDetection {
                points: d.bounding_box.points,
                text: d.text_prediction.text,
                confidence: d.text_prediction.confidence,
            })
        })
        .collect()
}

/// The class-keyed boxes for the first (only) image of a request, as `(class, box)` pairs
/// in class then answer order.
pub(crate) fn elements(backend: &str, model: &str, value: Value) -> Result<Vec<(String, NormBox)>> {
    let response: ElementsResponse =
        serde_json::from_value(value).map_err(|e| parse_error(backend, model, e))?;
    let Some(item) = response.data.into_iter().find(|item| item.index == 0) else {
        return Err(parse_error(backend, model, "the answer holds no result for the image"));
    };
    let mut out = Vec::new();
    for (class, boxes) in item.bounding_boxes {
        for b in boxes {
            finite(backend, model, &[b.x_min, b.y_min, b.x_max, b.y_max])?;
            out.push((class.clone(), b));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_answers_parse_into_quads() {
        let value = json!({"data": [{"index": 0, "text_detections": [{
            "text_prediction": {"text": "Quarterly", "confidence": 0.99},
            "bounding_box": {"points": [{"x": 0.1, "y": 0.2}, {"x": 0.3, "y": 0.2}, {"x": 0.3, "y": 0.25}, {"x": 0.1, "y": 0.25}]}
        }]}], "usage": {"images_size_mb": 0.04}});
        let found = text_detections("nim", "nvidia/nemotron-ocr-v1", value).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Quarterly");
        assert_eq!(found[0].points.len(), 4);
    }

    #[test]
    fn a_degenerate_quad_is_refused() {
        let value = json!({"data": [{"index": 0, "text_detections": [{
            "text_prediction": {"text": "x"},
            "bounding_box": {"points": [{"x": 0.1, "y": 0.2}]}
        }]}]});
        let err = text_detections("nim", "m", value).unwrap_err();
        assert!(matches!(err, VisionError::Parse { .. }), "{err}");
    }

    #[test]
    fn element_answers_flatten_by_class() {
        let value = json!({"data": [{"index": 0, "bounding_boxes": {
            "title": [{"x_min": 0.1, "y_min": 0.1, "x_max": 0.5, "y_max": 0.2, "confidence": 0.9}],
            "table": [{"x_min": 0.1, "y_min": 0.3, "x_max": 0.9, "y_max": 0.6, "confidence": 0.7}]
        }}]});
        let found = elements("nim", "m", value).unwrap();
        let classes: Vec<&str> = found.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(classes, vec!["table", "title"]);
    }

    #[test]
    fn the_body_carries_one_inline_image() {
        let body = body("data:image/png;base64,AAAA", Some(&["word"]));
        assert_eq!(
            body,
            json!({"input": [{"type": "image_url", "url": "data:image/png;base64,AAAA"}], "merge_levels": ["word"]})
        );
    }
}
