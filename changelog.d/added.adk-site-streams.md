- **Streams and scenario S19 in `adk-site`** (spec 3.5 §7.8.3, §11, Experimental): `StreamLane`
  opens a stream on a published story's stream field, signs each entry with the edge's key
  (`EntrySigner`, `Ed25519EntrySigner`; `sign_entry` and `verify_entry` use the AWP 2.0 detached
  JWS over JCS), serves entries at once (`UpdateEngine::stream_since`) and folds them into a
  signed release every five minutes and at the close; the renderer shows folded entries with
  their corrections. `tests/s19_constant_updates.rs` runs scenario S19 on a warped clock against
  `DirectoryHost` — scaled in every pull request, 50,000 products over 24 hours in the new
  nightly `site-soak` job.
