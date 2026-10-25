//! No page the kernel serves can call into the shell (spec 3.3 §5.5): no capability names a
//! remote page, and none grants a permission.

use serde_json::Value;

#[test]
fn no_capability_reaches_a_remote_page_or_grants_anything() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut files = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let capability: Value = serde_json::from_str(&text).unwrap();
        assert!(capability.get("remote").is_none(), "{} names remote pages", path.display());
        assert_eq!(
            capability["permissions"],
            serde_json::json!([]),
            "{} grants permissions",
            path.display()
        );
        files += 1;
    }
    assert!(files > 0, "no capability files in {}", dir.display());
}
