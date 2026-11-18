- **Modal Sandboxes behind `SandboxClient`** (`adk-sandbox`, feature `remote-modal`):
  `ModalClient` runs workspaces in Modal Sandboxes over Modal's gRPC API through `tonic`,
  from a vendored subset of Modal's protobuf definitions (Apache-2.0) with the generated code
  committed, so building needs no `protoc`. Sandboxes are created with a provider-side
  lifetime (`timeout_secs`) and terminated by `stop` or when the client drops; Modal kills a
  command at its timeout, every call carries a deadline, files move over the command router's
  stdin and stdout, and filesystem snapshots resume into new sandboxes. `ModalConfig::from_env`
  reads `MODAL_TOKEN_ID` and `MODAL_TOKEN_SECRET`, and `ModalClient::running_sandboxes` lists
  what runs in the app. The harness remote shell and `SandboxTool` run there unchanged;
  `live_modal_round_trip` and the harness's Modal runs are `#[ignore]`d live tests.
