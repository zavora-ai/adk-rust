- **`cargo adk new <name> --template vision`** (`cargo-adk`): a Document desk — a harness with
  the vision pack that reads each page in `inbox/` on the device with PP-OCRv6 into
  `tables/`, covers faces and personal text with RF-DETR and PP-OCRv6 before any image
  leaves, denies whole pages and asks at the terminal before a crop goes to the cloud model,
  with a scripted test that a held describe sends nothing when refused.
