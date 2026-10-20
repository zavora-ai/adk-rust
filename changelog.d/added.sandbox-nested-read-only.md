- **Read-only paths inside a read-write path** (`adk-sandbox`): when a policy's allowed
  paths nest, the most specific entry now decides, on Seatbelt and bubblewrap alike and
  whatever order the policy lists them in. A `ReadOnly` entry inside a `ReadWrite` entry
  — `.git` inside a workspace — refuses every write, creation, removal, rename, and link
  beneath it, and the writable directories above it cannot be renamed or removed, so it
  cannot be moved away from its path. Seatbelt previously kept the parent's write grant,
  and bubblewrap honoured the read-only entry only when the policy listed it after its
  parent. Spec 3.1 §10 S3.
