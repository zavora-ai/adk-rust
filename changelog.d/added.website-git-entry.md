- **Files and git as an entry point** (`adk-website`, `adk-server`, `adk-site`, spec 3.5
  §7.8.2, A-I24): a served site with a `repository.json` serves its content repository's
  signed hooks at `/hooks/{name}` on the edge. A push from an authorised committer becomes a
  kernel run that reads the changed content files at the pushed commit through a
  `repository::source::CommitSource` (`LocalGitSource` over `git`, `HttpCommitSource` over a
  forge's raw endpoint), drafts and previews the change through the site's pipeline, and
  waits for the owner's decision card (`site_repository_release`); a push from anyone else
  files one `inbound_message` signal. Each commit is taken once. `adk-server`'s
  `WebhookHook::forge` authenticates a hook with GitHub's `X-Hub-Signature-256` or GitLab's
  `X-Gitlab-Token` (`webhooks::ForgeSignature`) beside Standard Webhooks, and `adk-site`'s
  `content::parse_content_file` reads one content file as the content store does.
