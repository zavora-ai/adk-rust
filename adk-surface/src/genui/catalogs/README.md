# A2UI catalogs

The catalogs agent-drawn UI is checked against (spec 3.3 §6.1 `genui`, §7.4 rule 5). They are
embedded in the crate and never fetched.

| File | What | Source |
|---|---|---|
| `forms-1.0.json` | The `forms` 1.0 catalog, id `https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json` | Derived from the A2UI v0.9.1 basic catalog, `specification/v0_9_1/catalogs/basic/catalog.json` |
| `common_types-0.9.1.json` | The A2UI v0.9.1 common types the catalog refers to | `specification/v0_9_1/json/common_types.json`, unchanged |

## Upstream

- **Project:** A2UI, <https://github.com/a2ui-project/a2ui>
- **Commit:** `c07134caf2f63e3356c4439d248afc9eb04f310e` (2026-10-14)
- **Copyright:** Copyright 2024 Google LLC
- **License:** Apache License, Version 2.0, <https://www.apache.org/licenses/LICENSE-2.0>

## Changes from the basic catalog

| Change | Why |
|---|---|
| Removed the components `Image`, `Icon`, `Video`, `AudioPlayer` and `Modal` | Images and icons can draw a pixel-perfect fake decision card; media and modals cover the home |
| Removed the functions `openUrl`, `formatNumber`, `formatCurrency` and `formatDate` | A drawing opens no links; every display string is resolved on the server, which resolves `formatString` and `pluralize` only |
| Removed the `obscured` variant of `TextField` | A drawn form never asks for a secret; passwords go through Take over |
| Set `$id`, `catalogId`, `title` and `description` to the `forms` 1.0 catalog's | The catalog is not the basic catalog |

Every other component, function and definition is the upstream text, in the upstream order.
