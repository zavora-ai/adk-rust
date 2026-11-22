- **Live updates in agent-drawn UI** (`adk-surface`, feature `genui`): `patch_surface`'s result
  says what changed (`changed.components`, `changed.data`); a surface takes at most
  `MAX_PATCHES_PER_MINUTE` (30) patches a minute, and a patch past it is refused with what to
  do instead; `DrawnEnvelope::delta_from` gives the incremental A2UI messages between two
  versions (changed components, one `updateDataModel` per changed path) for transports that
  know which version a renderer holds. Envelopes, digests and AG-UI activities stay whole.
