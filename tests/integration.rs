//! Integration tests: verify the public API through the library entry point
//! (rather than the binary).
//!
//! Only after lib.rs was introduced in phase 0 could the end-to-end paths
//! (permission evaluation, command parsing, directory resolution) be covered
//! by external tests here. LLM network interaction is out of scope for
//! integration tests.

use hailux::permission::{PermissionAction, PermissionManager, PermissionMode, PermissionRule};
use hailux::resolve_work_dir;

#[test]
fn resolve_work_dir_returns_canonical_cwd() {
    let dir = resolve_work_dir(None).expect("resolve cwd");
    assert!(
        dir.is_absolute(),
        "work dir should be canonicalized to absolute"
    );
    // On Windows, canonicalize adds a \\?\ verbatim prefix; current_dir does not.
    // Strip it on both sides before comparing (same normalization rule as the
    // keys in session::manager)
    fn strip_verbatim(p: std::path::PathBuf) -> std::path::PathBuf {
        let s = p.to_string_lossy().to_string();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            std::path::PathBuf::from(stripped)
        } else {
            p
        }
    }
    let cwd = std::env::current_dir().expect("current dir");
    assert_eq!(strip_verbatim(dir), strip_verbatim(cwd));
}

#[test]
fn permission_manager_modes_and_rules() {
    let rules = vec![
        PermissionRule {
            permission: "shell".to_string(),
            pattern: "rm *".to_string(),
            action: PermissionAction::Deny,
        },
        PermissionRule {
            permission: "read".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Allow,
        },
    ];
    let pm = PermissionManager::new(PermissionMode::Normal, rules);

    // Yolo mode switching
    pm.set_mode(PermissionMode::Yolo);
    assert_eq!(pm.mode(), PermissionMode::Yolo);
    pm.set_mode(PermissionMode::Normal);
    assert_eq!(pm.mode(), PermissionMode::Normal);

    // auto_deny policy switch (web disconnect fallback and non-interactive mode rely on it)
    pm.set_auto_deny(true);
    pm.set_auto_deny(false);
}
