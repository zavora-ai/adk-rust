# Publishing from Git

A served AWP website takes content from its content repository: a push from an authorised
committer arrives through a signed webhook and proposes a release, which waits for the
owner's decision like a change from any other entry point (spec 3.5 §7.8.2, A-I24).
`adk-website`'s `repository` module implements it; a served site turns it on with a
`repository.json` in the project.

> **Note:** `adk-website` is Experimental. No forge is preferred: GitHub, GitLab and any host
> that signs Standard Webhooks reach the same entry point.

## What happens to a push

| Step | Where | Result |
|------|-------|--------|
| 1. Delivery | `POST /hooks/{name}` on the site's edge (`adk-server`'s `WebhookIngress`) | A bad signature answers `401`; a good one becomes one durable run keyed `hook:{name}:{delivery id}` in the site's kernel |
| 2. Push | `website.repository.push` (`repository::REPOSITORY_WORKFLOW`) | Anything but a push to the configured branch is ignored; a commit taken before answers `duplicate` |
| 3. Committer | `RepositoryConfig::committer` | A push from no listed committer files one `inbound_message` signal for the back office, with no account name and no file, and authors nothing |
| 4. Files | A `CommitSource` | The changed `content/<kind>/<slug>/<language>.md` files are read at the pushed commit; a removed one withdraws its item (410 Gone) |
| 5. Checks | The site's `PublishingPipeline`: draft, the per-change checks, the preview with page diffs | A held or refused change is recorded with the gate's summary; nothing publishes |
| 6. Decision | The owner's decision card (`site_repository_release`) in the kernel's approval inbox | The owner publishes or declines it wherever the owner's channels show the inbox's cards |
| 7. Release | The pipeline's approval chain (A-I23), release, publish, announce, measure | The release records the committer as its proposer and `repository` as its job |

Every push the site takes is kept in `data/repository/pushes.json` with what became of it:
`ignored`, `signalled`, `refused`, `held`, `proposed`, `approving`, `published`,
`scheduled` or `declined`.

## Configuration

`repository.json` sits in the project beside `site.toml`. Secrets are `env:VAR` or
`file:PATH`, never the secret itself (A-I19).

```json
{
  "branch": "main",
  "source": { "git": "../juniper-content", "fetch": "origin" },
  "hooks": [
    { "name": "github", "forge": "github", "secret": "env:JUNIPER_GITHUB_WEBHOOK_SECRET" },
    { "name": "gitlab", "forge": "gitlab", "secret": "env:JUNIPER_GITLAB_WEBHOOK_TOKEN" },
    { "name": "newsdesk", "forge": "signed", "secret": "env:JUNIPER_NEWSDESK_WHSEC" }
  ],
  "committers": [
    { "id": "wanjiru", "account": "github:wanjiru-k" },
    { "id": "otieno", "account": "gitlab:otieno" },
    { "id": "newsdesk", "key": "newsdesk" }
  ]
}
```

| Member | Holds |
|--------|-------|
| `branch` | The branch whose pushes propose releases; `main` by default |
| `source` | Where files are read: `git` (a repository on this machine, relative to the project, with an optional `fetch` remote) or `http` (a raw-file URL template, with an optional `auth` header) |
| `hooks` | Each hook's `name` (its path, `/hooks/<name>`), its `forge` and its `secret` |
| `committers` | Each committer's `id`, as the charter's governance names the person, and either an `account` (`<hook>:<username>` on a `github` or `gitlab` hook) or a `key` (a `signed` hook only that committer holds) |

### Hooks

| `forge` | Authenticated by | Delivery id | The committer |
|---------|------------------|-------------|---------------|
| `github` | `X-Hub-Signature-256`: HMAC-SHA256 of the raw body with the secret | `X-GitHub-Delivery` | The listed account that pushed: `sender.login`, else `pusher.name` |
| `gitlab` | `X-Gitlab-Token` equal to the secret token | `X-Gitlab-Event-UUID`, else `Idempotency-Key` | The listed account that pushed: `user_username` |
| `signed` | Standard Webhooks (`webhook-id`, `webhook-timestamp`, `webhook-signature`) with a `whsec_` key | `webhook-id` | The one committer whose `key` the hook is |

The plain signed form is a push document any host can send, such as a `post-receive` hook on
a server of your own:

```json
{
  "repository": "juniper/site",
  "ref": "refs/heads/main",
  "before": "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
  "after": "9f2c4e1b7a3d5c6e8f0a1b2c3d4e5f6a7b8c9d0e",
  "commits": [
    { "id": "9f2c4e1b7a3d5c6e8f0a1b2c3d4e5f6a7b8c9d0e", "added": [], "modified": ["content/pages/opening-hours/en.md"], "removed": [] }
  ]
}
```

GitHub and GitLab sign no timestamp, so a captured delivery verifies again later. The
ingress keys each run by the delivery id, and the entry takes each commit once: a commit an
authorised committer pushed is never proposed twice, and a commit only signalled is signalled
once on its hook and stays open to a listed committer's push.

### Sources

| `source` | Reads | Lists a push's changes |
|----------|-------|------------------------|
| `{ "git": "<dir>", "fetch": "<remote>" }` | `repository::source::LocalGitSource`: `git cat-file` in the repository, fetching from the remote first when the commit is missing | `git diff --name-status` between the push's two commits, so a push of many commits is read whole |
| `{ "http": "<template>", "auth": { "header": "...", "value": "env:..." } }` | `repository::source::HttpCommitSource`: `GET` on the template; `404` is a file the commit does not hold. Redirects are not followed, so the credential reaches no other host | The push's own lists |

| Forge | `http` template |
|-------|-----------------|
| GitHub | `https://raw.githubusercontent.com/{repository}/{commit}/{path}` |
| GitLab | `https://gitlab.com/api/v4/projects/{repository_escaped}/repository/files/{path_escaped}/raw?ref={commit}` |

A source refuses a commit id that is not 40 or 64 hexadecimal digits and a path that is
absolute or climbs with `..`, and reads at most 4 MiB of one file. A source of your own
implements `CommitSource` and passes `commit_source_conformance`; give it to
`RepositoryEntry::open` through `RepositoryParts::source`.

## Content files

A content file is a Markdown body with front matter, at the path the site's own content
repository uses (`adk_site::content::parse_content_file`):

```markdown
---
kind: page
title: Opening hours
summary: When the bakery is open.
---
We are open from seven to seven.
```

`content/pages/opening-hours/en.md` is the item `pages/opening-hours/en`. Media and other
files in the push are not read; a content file that does not parse refuses the whole push,
with the reason in its record.

## Approval

The decision card names the repository, the branch, the commit, the committer, the items and
the preview's page diffs. When the owner publishes it, the owner's vote is recorded on the
preview; when the charter names an approval chain for `publish`, the release waits as
`approving` until every role has approved, each a different person and none of them the
committer (A-I23). A declined or expired card publishes nothing.

## Related

- [The AWP Website Product](adk-website.md): the served site and its other entry points
- [Sites and AWP from the Command Line](site-cli.md): `adk-rust site` publishes from the working copy as the person at the terminal
- [Constantly Updated Sites](constant-updates.md): feeds, the entry point for systems that send records rather than files
