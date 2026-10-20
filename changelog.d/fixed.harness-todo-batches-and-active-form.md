- **Todo updates in one batch all land, and `activeForm` is one name** (`adk-harness`):
  `write_todos` and `update_todo` calls in the same model turn each read the list the
  previous one wrote, so two `update_todo` calls no longer answer the same revision and
  drop an update. `write_todos` declares `activeForm` (still accepting `active_form`),
  the stored list keeps `activeForm`, and the tool response returns the items in the
  stored shape, `activeForm` included (spec 3.1 §7.6).
