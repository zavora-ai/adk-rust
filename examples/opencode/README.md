# OpenCode Go and Zen

Set `OPENCODE_API_KEY`, then run `cargo run` from this directory. Running the example sends a model request using the Go account's quota.

The application supplies its own user agent and stable conversation ID. Reuse that ID for auxiliary requests in the same conversation. Start a different ID for a new conversation.

To use Zen, change the service to `OpenCodeService::Zen` and supply a key with Zen access. Zen usage is billed separately from Go.
