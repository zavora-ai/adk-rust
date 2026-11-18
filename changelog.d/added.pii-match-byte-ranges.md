- **`PiiRedactor::find` reports personal data as byte ranges** (`adk-guardrail`): each
  `PiiMatch` names the type and its range in the original text, so OCR words can be mapped
  to their boxes in an image. The docs state that image parts pass through `PiiRedactor`
  unchanged and point to `adk-vision`'s `ImagePrivacyGuardrail`.
