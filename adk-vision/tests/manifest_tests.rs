//! Model manifests and licences (spec 3.7 §7.4, S6, V-I10, V-I11, V-I12).

use adk_vision::{
    AcceptanceStore, Digest, FileAcceptanceStore, InMemoryAcceptanceStore, LicenceAcceptance,
    LicenceClass, LicenceGate, LicencePolicy, ManifestSet, ModelManifest, VerifiedModel,
    VisionError,
};
use serde_json::json;

fn manifest(id: &str, class: &str, path: &str) -> serde_json::Value {
    json!({
        "id": id,
        "task": "detect",
        "source": {"kind": "huggingFace", "repo": "owner/name", "revision": "eae21cee0687a91bcf9fa071605c48d7705d2d91"},
        "files": [{"path": path, "sha256": Digest::of(b"w"), "size": 1}],
        "licence": {"id": "LicenseRef-Test", "url": "https://example.org/licence", "class": class},
        "runtime": {"kind": "ort", "opset": 17},
        "io": {"input": {"width": 576, "height": 576, "letterbox": "center"}, "labels": ["seal"]}
    })
}

#[test]
fn the_builtin_manifest_holds_no_non_commercial_or_copyleft_model() {
    let builtin = ManifestSet::builtin().unwrap();
    for m in builtin.iter() {
        m.validate().unwrap();
        assert!(
            matches!(m.licence.class, LicenceClass::Permissive | LicenceClass::UseRestricted),
            "{} is {} and cannot be built in",
            m.id,
            m.licence.class
        );
    }
}

#[test]
fn user_manifests_load_from_a_directory_and_ids_are_unique() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.json"),
        manifest("my-rf-detr", "permissive", "model.onnx").to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.json"),
        manifest("my-nc", "nonCommercial", "nc.onnx").to_string(),
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not a manifest").unwrap();
    let set = ManifestSet::builtin().unwrap().with_dir(dir.path()).unwrap();
    assert!(set.get("my-rf-detr").is_some());
    assert_eq!(set.get("my-nc").unwrap().licence.class, LicenceClass::NonCommercial);
    assert_eq!(set.get("my-rf-detr").unwrap().io.labels, vec!["seal".to_string()]);

    std::fs::write(
        dir.path().join("c.json"),
        manifest("my-rf-detr", "permissive", "other.onnx").to_string(),
    )
    .unwrap();
    let err = ManifestSet::new().with_dir(dir.path()).unwrap_err();
    assert!(err.to_string().contains("already uses this id"), "{err}");
}

#[test]
fn manifests_pin_commits_and_relative_paths() {
    let mut branch = manifest("m", "permissive", "model.onnx");
    branch["source"]["revision"] = json!("main");
    for bad in [
        branch,
        manifest("m", "permissive", "../escape.onnx"),
        manifest("m", "permissive", "/etc/passwd"),
        manifest("m", "permissive", "a//b.onnx"),
        manifest("Upper Case", "permissive", "model.onnx"),
    ] {
        let err = ModelManifest::from_json(&bad.to_string()).unwrap_err();
        assert!(matches!(err, VisionError::Manifest { .. }), "{err}");
    }
}

#[test]
fn acceptance_binds_to_the_licence_a_person_read() {
    let restricted =
        ModelManifest::from_json(&manifest("sam3-local", "useRestricted", "sam3.onnx").to_string())
            .unwrap();
    let store = InMemoryAcceptanceStore::new();
    let policy = LicencePolicy::new();
    let gate = LicenceGate::new(&policy, &store);
    let err = gate.check(&restricted).unwrap_err();
    assert!(err.to_string().contains("licence acceptance required for model `sam3-local`"));
    assert!(err.to_string().contains("https://example.org/licence"));

    store.record(LicenceAcceptance::of(&restricted, "alice@example.com")).unwrap();
    gate.check(&restricted).unwrap();

    // The licence changed: the old acceptance no longer covers it.
    let mut changed = restricted.clone();
    changed.licence.url = "https://example.org/licence-v2".into();
    assert!(matches!(
        gate.check(&changed).unwrap_err(),
        VisionError::LicenceAcceptanceRequired { .. }
    ));
}

#[test]
fn non_commercial_and_copyleft_models_load_only_when_named() {
    for class in ["nonCommercial", "copyleft"] {
        let m =
            ModelManifest::from_json(&manifest("restricted", class, "w.onnx").to_string()).unwrap();
        let store = InMemoryAcceptanceStore::new();
        let none = LicencePolicy::new();
        assert!(matches!(
            LicenceGate::new(&none, &store).check(&m).unwrap_err(),
            VisionError::LicenceNotAllowed { .. }
        ));
        // A recorded acceptance is not enough for these classes.
        store.record(LicenceAcceptance::of(&m, "alice@example.com")).unwrap();
        assert!(LicenceGate::new(&none, &store).check(&m).is_err());
        let named = LicencePolicy::new().allowing("restricted");
        LicenceGate::new(&named, &store).check(&m).unwrap();
    }
}

#[test]
fn file_acceptances_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let m =
        ModelManifest::from_json(&manifest("sam3-local", "useRestricted", "sam3.onnx").to_string())
            .unwrap();
    FileAcceptanceStore::in_project(dir.path())
        .record(LicenceAcceptance::of(&m, "alice@example.com"))
        .unwrap();
    let reopened = FileAcceptanceStore::in_project(dir.path());
    let all = reopened.acceptances().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].principal, "alice@example.com");
    assert!(dir.path().join(".adk/vision/licences.json").exists());
    let policy = LicencePolicy::new();
    LicenceGate::new(&policy, &reopened).check(&m).unwrap();
}

#[test]
fn a_size_mismatch_is_refused_before_hashing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("w.onnx"), b"longer than one byte").unwrap();
    let m = ModelManifest::from_json(&manifest("m", "permissive", "w.onnx").to_string()).unwrap();
    let (policy, store) = (LicencePolicy::new(), InMemoryAcceptanceStore::new());
    let err =
        VerifiedModel::check_dir(&m, dir.path(), &LicenceGate::new(&policy, &store)).unwrap_err();
    assert!(matches!(err, VisionError::SizeMismatch { expected: 1, actual: 20, .. }), "{err}");
}
