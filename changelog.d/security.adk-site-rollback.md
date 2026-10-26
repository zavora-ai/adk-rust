- **Rollback trusts the site log, not the host** (`adk-site`): `PublishingPipeline::rollback`
  checks every manifest it reads from the host against the digest the site log recorded, so
  a forged manifest rolls nothing back; when the host cannot roll back it republishes the
  signed files from a copy with the signed digest (the host's current files, the last build,
  or a rebuild of the content history), and fails rather than publish anything unsigned.
- **`DirectoryHost` objects are read-only** (`adk-site`): release directories hard-link
  read-only stored files on Unix (copies elsewhere), so a write through one served file no
  longer changes every release that shares it; a stored file whose bytes no longer match its
  digest is replaced the next time a release carries it.
