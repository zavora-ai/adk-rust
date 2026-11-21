- **Calibration and measurement** (`adk-vision`): `Calibration::from_fiducial` turns a
  fiducial of known size in the measured plane into pixels per millimetre with an uncertainty,
  at any rotation; `Calibration::measure` reports a mask's, polygon's or box's maximum length
  (maximum Feret diameter) and area with their propagated uncertainties, valid only in the
  calibrated plane. The governed `vision_measure` tool segments the fiducial and the objects,
  measures them, says where the result holds, and writes the measurements to the answer record
  the annotated viewer draws. See `docs/official_docs/vision/measure.md`.
