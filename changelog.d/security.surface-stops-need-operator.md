- **Stop all and Resume all on a mounted web home can be kept for the operator**
  (`adk-surface`, `adk-cli`): `SurfaceEngineBuilder::stops_need(group)` refuses both commands,
  from every surface, to a caller outside `group` with `403 surface.operator_only`. Both freeze
  and unfreeze every run of the source, so before, any caller a server admitted could stop
  every other user's runs or lift an operator's freeze through `adk_surface::web::router`; the
  documented library mount now builds its engine with `stops_need("operator")`. `adk-rust serve`
  and `adk-rust up` set it with the `operator` scope in place of their own route layer.
