# A2UI catalogs

The catalogs agent-drawn UI is checked against (spec 3.3 §6.1 `genui`, §7.4 rule 5). They are
embedded in the crate and never fetched.

| File | What | Source |
|---|---|---|
| `forms-1.0.json` | The `forms` 1.0 catalog, id `https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json` | Derived from the A2UI v0.9.1 basic catalog, `specification/v0_9_1/catalogs/basic/catalog.json` |
| `common_types-0.9.1.json` | The A2UI v0.9.1 common types the catalog refers to | `specification/v0_9_1/json/common_types.json`, unchanged |
| `forms-1.0.a2ui-1.0.json` | The same `forms` 1.0 catalog as an A2UI 1.0 catalog document (`protocolVersion: "1.0"`), which `Catalog::document_for(A2uiVersion::V1_0)` serves | Derived from the A2UI 1.0 basic catalog, `catalogs/basic/v1/catalog.json` at `de4a91688524`, by `tools/make_forms_v1.py` |
| `common_types-1.0.json` | The A2UI 1.0 common types | `specification/v1_0/json/common_types.json` at `de4a91688524`, unchanged |
| `catalog_definition-1.0.json` | The A2UI 1.0 catalog meta-schema, which the tests check every 1.0 document against | `specification/v1_0/json/catalog_definition.json` at `de4a91688524`, unchanged |

## Upstream

- **Project:** A2UI, <https://github.com/a2ui-project/a2ui>
- **Commit:** `c07134caf2f63e3356c4439d248afc9eb04f310e` (2026-10-14) for v0.9.1; `de4a916885246aa34971e91459c9b116a7634df5` (2026-11-18) for 1.0
- **Copyright:** Copyright 2024 Google LLC
- **License:** Apache License, Version 2.0, <https://www.apache.org/licenses/LICENSE-2.0>

## Changes from the basic catalog

| Change | Why |
|---|---|
| Removed the components `Image`, `Icon`, `Video`, `AudioPlayer` and `Modal` | Images and icons can draw a pixel-perfect fake decision card; media and modals cover the home |
| Removed the functions `openUrl`, `formatNumber`, `formatCurrency` and `formatDate` | A drawing opens no links; every display string is resolved on the server, which resolves `formatString` and `pluralize` only |
| Removed the `obscured` variant of `TextField` | A drawn form never asks for a secret; passwords go through Take over |
| Set `$id`, `catalogId`, `title` and `description` to the `forms` 1.0 catalog's | The catalog is not the basic catalog |
| 1.0 only: kept `Text`'s `h1`–`h5` variants, which 1.0's basic catalog dropped; replaced the instructions' examples with the rule that a button dispatches only a declared action | The render tools draw titles as headings; the examples show components `forms` leaves out |
| 0.9.1 only: added `TextField.placeholder` and `Slider.steps` from 1.0 | Components written in 1.0's syntax keep their meaning when read into the internal form |

Every other component, function and definition is the upstream text, in the upstream order.
