- **Catalogs as A2UI 1.0 documents** (`adk-surface`, feature `genui`):
  `Catalog::document_for(A2uiVersion)` writes `forms` as an A2UI 1.0 catalog (derived from the
  1.0 basic catalog, headings kept, no obscured field) and an extension catalog as a 1.0
  catalog of only the components it adds, with `protocolVersion`, `instructions` (usage lines
  and `Catalog::with_instructions`) and `allowedParents`/`allowedChildren`
  (`ExtensionComponent::allowed_parents`, `allowed_children`). Both pass the pinned A2UI 1.0
  meta-schema. `GET /api/surface/genui/catalogs?version=1.0` serves them. The internal
  catalog accepts 1.0's `TextField.placeholder` (checked like a label) and `Slider.steps`.
