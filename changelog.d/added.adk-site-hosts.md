- **`adk-site` host adapters** (features `host-s3`, `host-gcs`, `host-azure`,
  `host-cloudflare`, `host-netlify`, `host-github-pages`, `host-rsync`, spec 3.5 §7.8,
  §7.8.3, Experimental): `ObjectStoreHost` over an `ObjectStore` with a prefix-and-pointer
  layout (`Atomicity::PrefixPointer`, pointer written last) or an in-place website layout with
  a private release archive (`Atomicity::None`), delta uploads with server-side copies and a
  `CachePurge` hook; `S3Store` for any S3-compatible store with Signature Version 4 and
  `CloudFrontInvalidation`; `GcsStore`; `AzureBlobStore` with Shared Key, bearer or SAS
  authorisation and the `$web` static website; Cloudflare R2 behind a pointer-following
  Worker with `CloudflarePurge`; `NetlifyHost` over file-digest deploys; `GitPagesHost`,
  one commit per release pushed to a pages branch; and `RsyncHost`, hard-linked release
  directories over SSH switched by a symlink. Each declares its `HostLimits`, takes its
  credential from a `CredentialBroker` through `HostCredential`, and passes the host
  conformance suite.
