# Site hosts

`adk-site` publishes a signed release to any static host through the `SiteHost` trait (spec 3.5
§7.8 "Hosts", §7.8.3). `DirectoryHost` is built in; the adapters below arrive as features. Every
adapter uploads a release's files, then its manifest, then the log's tiles, then the checkpoint,
so a reader never sees a checkpoint for files that are not there; declares its `HostLimits`; and
passes the host conformance suite.

> **Note:** `adk-site` is **Experimental**. Its API may change while spec 3.5 lands.

## Adapters

| Feature | Type | Atomicity | Publishes per hour | Redirects |
|---------|------|-----------|--------------------|-----------|
| `host-s3` | `S3Store` under `ObjectStoreHost`, `CloudFrontInvalidation` | `PrefixPointer`, or `None` in place | Unlimited | `HtmlMeta` (configurable) |
| `host-gcs` | `GcsStore` under `ObjectStoreHost` | `PrefixPointer`, or `None` in place | Unlimited | `HtmlMeta` (configurable) |
| `host-azure` | `AzureBlobStore` under `ObjectStoreHost` | `None` in place (`$web`), or `PrefixPointer` | Unlimited | `HtmlMeta` (configurable) |
| `host-cloudflare` | `r2_store`, `r2_host`, `CloudflarePurge` | `PrefixPointer` | Unlimited | `HtmlMeta` (configurable) |
| `host-netlify` | `NetlifyHost` | `Deploy` | 4 (the API allows 100 deploys a day) | `Netlify` (`_redirects`) |
| `host-github-pages` | `GitPagesHost` | `Deploy` | 10 (GitHub Pages' soft build limit) | `HtmlMeta` |
| `host-rsync` | `RsyncHost` | `Deploy` | Unlimited | `HtmlMeta` (configurable) |

```toml
[dependencies]
adk-site = { version = "3.0.0", features = ["host-s3", "host-netlify"] }
```

A host with `Atomicity::None` cannot serve a site whose batch window is under one minute
(spec 3.5 §7.8.3); onboarding compares the charter's update rate with `limits()`.

## Object storage layouts

`ObjectStoreHost` runs over any `ObjectStore` — a bucket or container with put, get, server-side
copy and delete — in one of two layouts.

| Layout | Keys | Served by |
|--------|------|-----------|
| `ObjectStoreHost::prefix_pointer(store)` | `releases/<seq>/…`, `previews/<seq>-<digest>/…`, `<dir>.index.json`, and `current.pointer` written last with `Cache-Control: no-cache, max-age=0` | An edge that reads the pointer and serves `/path` from `<pointer>/path` |
| `ObjectStoreHost::in_place(live, archive)` | The site at the root of `live`; the prefix layout in the private `archive` | The store's website endpoint (S3 website hosting, Azure `$web`, a GCS website bucket) |

A publish uploads only the files whose digest differs from the serving release and copies the
rest server-side. A rollback reads every file of the earlier release, checks it against the
release's manifest chain, and serves it with the log as it stands. `with_cache_purge` runs a
`CachePurge` after the switch; a failed purge is logged and does not fail the publish.

```rust
use std::sync::Arc;

use adk_core::{ActingContext, CredentialBroker, SubjectKind, resource_indicator};
use adk_site::host::object::ObjectStoreHost;
use adk_site::host::s3::{CloudFrontInvalidation, S3Store};
use adk_site::host::{HostCredential, HostTarget, SiteHost};
use adk_site::release::StagedRelease;

async fn publish(
    broker: Arc<dyn CredentialBroker>,
    release: &StagedRelease,
) -> adk_site::Result<()> {
    let acting = ActingContext::derived("example-site", "publisher", "owner", SubjectKind::Human);
    let bucket = resource_indicator("https://example-site.s3.eu-west-1.amazonaws.com/")?;
    let cloudfront = resource_indicator("https://cloudfront.amazonaws.com/")?;
    let store = S3Store::aws(
        "eu-west-1",
        "example-site",
        HostCredential::new(broker.clone(), acting.clone(), bucket),
    );
    let host = ObjectStoreHost::prefix_pointer(Arc::new(store)).with_cache_purge(Arc::new(
        CloudFrontInvalidation::new("E2EXAMPLE", HostCredential::new(broker, acting, cloudfront)),
    ));
    host.publish(release, HostTarget::Production).await?;
    Ok(())
}
```

### Cloudflare R2

`r2_host` keeps releases in R2 under the prefix layout. A Worker bound to the bucket as `SITE`
serves the release the pointer names:

```js
let pointer = { dir: null, at: 0 };
export default {
  async fetch(request, env) {
    if (Date.now() - pointer.at > 5000) {
      const object = await env.SITE.get("current.pointer");
      pointer = { dir: object ? (await object.text()).trim() : null, at: Date.now() };
    }
    if (!pointer.dir) return new Response("Not found", { status: 404 });
    let key = new URL(request.url).pathname.slice(1);
    if (key === "" || key.endsWith("/")) key += "index.html";
    const object = await env.SITE.get(`${pointer.dir}/${key}`);
    if (!object) return new Response("Not found", { status: 404 });
    const headers = new Headers();
    object.writeHttpMetadata(headers);
    return new Response(object.body, { headers });
  },
};
```

`CloudflarePurge` purges the zone's changed URLs (a file `/a/index.html` also purges `/a/`) in
batches of 30, and the whole zone above 300 URLs.

## Netlify, GitHub Pages and servers

| Host | Publish | Rollback | `fetch` reads |
|------|---------|----------|---------------|
| `NetlifyHost` | A deploy from SHA-1 file digests; only the files Netlify asks for are uploaded; drafts for previews; a ready deploy that is not live is restored | The earlier deploy's files, checked through its permalink, deployed again with the current log | The site URL |
| `GitPagesHost` | One commit on top of the pages branch, written with `git fast-import` and pushed without force; `.nojekyll` and an optional `CNAME` | A commit of the earlier release's blobs, checked against its manifest | The pages branch |
| `RsyncHost` | `rsync --checksum --link-dest` into a staging directory, unchanged files hard-linked, the log linked forward, renamed into `releases/<seq>/`, `current` swapped with one rename | A new directory hard-linked from the earlier release after its files are hashed on the server | `<root>/current` |

Point the web server's document root at `<root>/current` for `RsyncHost`. The server needs
`rsync`, a POSIX shell, and `sha256sum` or `shasum`. Host keys are always checked; pin them with
`with_known_hosts`.

## Credentials

Every adapter takes a `HostCredential`: a `CredentialBroker`, the acting agent and the
provider's resource. The adapter asks the broker for a token on each request and sends it only
to requests under the token's audience (A-I19).

| Adapter | Secret |
|---------|--------|
| `S3Store`, R2, `CloudFrontInvalidation` | JSON `{"accessKeyId", "secretAccessKey", "sessionToken"?}` |
| `GcsStore` | An OAuth 2.0 access token for `https://storage.googleapis.com/` |
| `AzureBlobStore` | The account key (`AzureAuth::SharedKey`), an Entra ID token (`AzureAuth::Bearer`), or a SAS query string (`AzureAuth::Sas`) |
| `CloudflarePurge`, `NetlifyHost` | An API token |
| `GitPagesHost` | A token sent to `git` as an `http.extraHeader` in its environment |
| `RsyncHost` | An OpenSSH private key, handed to `ssh` in a file only this user can read, removed after each command |

## Conformance

`adk_site::host::conformance::run_all` checks the publication order, atomicity under
concurrent readers, exact rollback, the redirect round trip and delta publication. The adapters'
tests run it against provider fakes that check request signatures independently, a local
`git` remote and a local `rsync` target. The `#[ignore]`d tests in
`adk-site/tests/host_adapters/` run it against MinIO or S3, Azurite or Azure, a Cloud Storage
bucket, Netlify, a GitHub repository and an SSH server; each names the environment variables it
reads.

```bash
cargo nextest run -p adk-site --features host-s3,host-gcs,host-azure,host-cloudflare,host-netlify,host-github-pages,host-rsync
```
