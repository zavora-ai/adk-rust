- **Document readers for `adk-vision`** (`adk-vision`; spec 3.7 §7.6, §7.7, S8): `PpOcrReader`
  (`ocr`) reads text lines and words with PP-OCRv6 through `oar-ocr` 0.10.3 on ONNX Runtime;
  `PaddleOcrVlReader` (`ocr-vl`, `ocr-vl-metal`) reads layout blocks, tables and formulas
  with PaddleOCR-VL-1.6 and PP-DocLayoutV3 on Candle; `NimDocumentReader` (`nim`) composes
  NVIDIA's OCR, page-element, table-structure and chart-element endpoints, takes only images
  the egress gate released, and maps NVIDIA's statuses to `AdkError` categories. All three
  answer one `DocumentPage`, published as `schema/document-page.json`. The built-in manifest
  pins the eight PP-OCRv6, PaddleOCR-VL-1.6 and PP-DocLayoutV3 models by revision and
  SHA-256. A read records a page unit beside its image, and a remote backend with no price
  is billed under its host.
