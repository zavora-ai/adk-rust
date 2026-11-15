- **Extension catalogs for agent-drawn UI** (`adk-surface`, feature `genui`; spec 3.3 §6.1,
  decision 11 amended): `Catalog::extend` builds a catalog that is `forms` 1.0 plus another
  crate's `ExtensionComponent`s, under its own id, so `adk-ui` 3 can add charts, metrics,
  pages and scenes without a second render stack. An extension never loosens `forms`: its
  components only show (a schema that declares `action`, `event`, `checks`, `$defs`,
  `functions` or a URI format is refused, and so is an instance that carries an action, an
  event or a check function), every string in them and every string and key of the data
  they bind is checked by rule 5 however it nests, each one says what the home and the
  terminal show for it (`ExtensionComponent::home`, in forms components and charts only,
  every child it holds shown exactly once), and their resolved properties are capped at
  `MAX_RESOLVED_BYTES` across the drawing. A chart's strings, its limits (eight series, a
  pie of at most eight slices, none negative) and the summary the server writes for it are
  checked too. A drawing is checked again against its own catalog wherever it is shown,
  sent or carried: register an extension catalog with `SurfaceEngineBuilder::with_catalog`
  and `GenuiAgUi::with_catalog`; a drawing in a catalog that is not registered is refused
  ("this home does not render the catalog …"), and AG-UI drops it. `DrawToolset::new` keeps
  the render tools' names in every catalog, tells the model about the catalog's components
  in `render_screen`, and refuses to patch a drawing from a catalog it does not draw in;
  `DrawToolset::catalog`, `SurfaceEngine::catalogs`, `Catalogs`, `capabilities_with` and
  `A2uiCapabilities::of` list them, and `GET /api/surface/genui/capabilities` shows every
  registered catalog. The chart wire types (`DrawnChart`, `ChartKind`, `ChartAxis`,
  `ChartSeries`, `ChartTick`) compile in every build.
