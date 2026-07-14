use std::path::PathBuf;

use bangbang_firecracker_capability_audit::{
    AuditMode, CAPABILITY_INVENTORY_PATH, SOURCE_MANIFEST_PATH, read_capability_inventory,
    read_source_manifest, validate,
};

#[test]
fn checked_inventory_is_valid_for_delivery() {
    let tool_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = tool_root
        .parent()
        .and_then(|tools| tools.parent())
        .expect("tool package must be nested under the repository tools directory");
    let manifest = read_source_manifest(&repository_root.join(SOURCE_MANIFEST_PATH))
        .expect("checked source manifest must parse");
    let inventory = read_capability_inventory(&repository_root.join(CAPABILITY_INVENTORY_PATH))
        .expect("checked capability inventory must parse");

    validate(&manifest, &inventory, repository_root, AuditMode::Delivery)
        .expect("checked inventory must satisfy delivery-time invariants");
}
