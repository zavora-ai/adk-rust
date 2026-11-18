//! Regenerates the Rust code of adk-sandbox's Modal protocol subset from
//! `adk-sandbox/proto/modal/modal_proto/*.proto`.
//!
//! The output is committed, so building adk-sandbox needs no `protoc`; this program does
//! (on `PATH`, or named by `PROTOC`). From the repository root:
//!
//! ```bash
//! cargo run --manifest-path adk-sandbox/proto/modal/codegen/Cargo.toml
//! ```
//!
//! | Output | Contents |
//! |--------|----------|
//! | `adk-sandbox/src/workspace/remote/modal/proto/` | Messages and clients, for `ModalClient` |
//! | `adk-sandbox/tests/modal_mock/` | Messages and servers, for the contract tests' mock |

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize()?;
    let proto_dir = crate_dir.join("proto/modal");
    let protos = [
        proto_dir.join("modal_proto/api.proto"),
        proto_dir.join("modal_proto/task_command_router.proto"),
    ];
    let includes = [proto_dir.clone()];

    tonic_prost_build::configure()
        .build_server(false)
        .build_transport(false)
        .emit_rerun_if_changed(false)
        .out_dir(crate_dir.join("src/workspace/remote/modal/proto"))
        .compile_protos(&protos, &includes)?;

    tonic_prost_build::configure()
        .build_client(false)
        .build_transport(false)
        .emit_rerun_if_changed(false)
        .out_dir(crate_dir.join("tests/modal_mock"))
        .compile_protos(&protos, &includes)?;
    Ok(())
}
