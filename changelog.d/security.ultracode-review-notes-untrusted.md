- **`ultracode` delivers review notes as untrusted text** (`adk-cli`, spec 3.1 §9.2 I32,
  I47): the implementer's revise prompt carries the reviewers' notes inside an
  `<untrusted-content source="review">` block whose closing tag a note cannot forge, as
  instruction files and fetched pages arrive, instead of appending them to its instructions.
  A path component `security` makes a change security-sensitive for the review router.
