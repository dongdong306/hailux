//! 集成测试：通过库入口（而非二进制）验证公开 API。
//!
//! 阶段 0 建立 lib.rs 后，端到端路径（权限评估、命令解析、目录解析）
//! 才能在这里被外部测试覆盖。LLM 网络交互不在集成测试范围。

use hailux::permission::{PermissionAction, PermissionManager, PermissionMode, PermissionRule};
use hailux::resolve_work_dir;

#[test]
fn resolve_work_dir_returns_canonical_cwd() {
    let dir = resolve_work_dir(None).expect("resolve cwd");
    assert!(
        dir.is_absolute(),
        "work dir should be canonicalized to absolute"
    );
    // Windows 的 canonicalize 会带 \\?\ verbatim 前缀，current_dir 不带；
    // 比较前统一剥离（与 session::manager 的键归一化规则一致）
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
            permission: "bash".to_string(),
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

    // Yolo 模式切换
    pm.set_mode(PermissionMode::Yolo);
    assert_eq!(pm.mode(), PermissionMode::Yolo);
    pm.set_mode(PermissionMode::Normal);
    assert_eq!(pm.mode(), PermissionMode::Normal);

    // auto_deny 策略开关（Web 断连兜底 / 非交互模式依赖同一机制）
    pm.set_auto_deny(true);
    pm.set_auto_deny(false);
}
