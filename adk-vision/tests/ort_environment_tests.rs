//! `ort` allows one ONNX Runtime environment per process. When code outside ADK-Rust built it
//! first, here `oar-ocr` on its own, `adk_core::onnx` names the fix, and once that environment is
//! installed every ADK-Rust crate and `oar-ocr` run on it.
//!
//! One test in its own binary: the environment is process state, so no other test may build it
//! first.

use ort::AsPointer;

#[test]
fn an_environment_built_outside_adk_is_named_then_installed_and_shared() {
    let foreign = oar_ocr::core::inference::initialize_ort_environment().unwrap();

    let refused = adk_core::onnx::ort_environment().unwrap_err().to_string();
    assert!(refused.contains("only one environment is allowed per process"), "{refused}");
    assert!(refused.contains("adk_core::onnx::set_ort_environment"), "{refused}");

    adk_core::onnx::set_ort_environment(foreign.clone()).unwrap();
    assert_eq!(adk_core::onnx::ort_environment().unwrap().ptr(), foreign.ptr());
    assert_eq!(
        oar_ocr::core::inference::initialize_ort_environment().unwrap().ptr(),
        foreign.ptr()
    );

    let twice = adk_core::onnx::set_ort_environment(foreign).unwrap_err().to_string();
    assert!(twice.contains("already installed"), "{twice}");
}
