pub mod bash_arity;
pub mod bash_readonly;

use std::sync::{Arc, Mutex};

/// 权限动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Deny,
    Ask,
}

impl PermissionAction {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "allow" => Self::Allow,
            "deny" => Self::Deny,
            _ => Self::Ask,
        }
    }
}

/// 权限模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// 默认模式：危险操作需要用户确认
    Normal,
    /// YOLO 模式：跳过所有权限检查
    Yolo,
}

impl PermissionMode {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "yolo" => Self::Yolo,
            _ => Self::Normal,
        }
    }
}

/// 单条权限规则
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: PermissionAction,
}

/// JSON 序列化用的规则结构
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SerializableRule {
    permission: String,
    pattern: String,
    action: PermissionAction,
}

impl serde::Serialize for PermissionAction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Ask => "ask",
        })
    }
}

impl<'de> serde::Deserialize<'de> for PermissionAction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_str(&s))
    }
}

/// 权限评估结果
#[derive(Debug, Clone)]
pub enum PermissionResult {
    /// 允许执行
    Allowed,
    /// 拒绝（附带原因）
    Denied(String),
    /// 需要询问用户
    NeedsAsk(PermissionRequest),
}

/// 权限请求（传递给 TUI 弹窗）
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// 权限类别: "bash", "read", "edit", "write", "mcp", "external_directory"
    pub permission: String,
    /// 匹配的 patterns（用于规则匹配）
    pub patterns: Vec<String>,
    /// 选择 "always" 时持久化的 patterns
    pub always_patterns: Vec<String>,
    /// 给用户看的简短描述
    pub description: String,
}

/// 用户对权限请求的回复
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReply {
    /// 允许这一次
    Once,
    /// 始终允许（本会话内同类操作不再询问）
    Always,
    /// 拒绝
    Deny,
}

struct PermissionInner {
    mode: PermissionMode,
    config_rules: Vec<PermissionRule>,
    session_rules: Vec<PermissionRule>,
    /// 当前会话 ID（用于 DB 持久化）
    session_id: Option<String>,
    /// 存储句柄（用于持久化 session 级规则）
    storage: Option<crate::storage::ChatStorage>,
    /// 无交互策略：需要询问（ask）的操作自动拒绝，而不是发弹窗。
    /// 供非交互模式使用（无 TUI 应答，避免工具调用挂起）。
    auto_deny: bool,
}

/// 线程安全的权限管理器
#[derive(Clone)]
pub struct PermissionManager {
    inner: Arc<Mutex<PermissionInner>>,
}

impl PermissionManager {
    pub fn new(mode: PermissionMode, config_rules: Vec<PermissionRule>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PermissionInner {
                mode,
                config_rules,
                session_rules: Vec::new(),
                session_id: None,
                storage: None,
                auto_deny: false,
            })),
        }
    }

    /// 绑定存储句柄（&self 版本，用于构造后绑定）
    pub fn with_storage_ref(&self, storage: crate::storage::ChatStorage) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.storage = Some(storage);
        }
    }

    pub fn mode(&self) -> PermissionMode {
        self.inner
            .lock()
            .map(|i| i.mode)
            .unwrap_or(PermissionMode::Normal)
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.mode = mode;
        }
    }

    /// 设置无交互策略：开启后需要询问的操作自动拒绝（不弹窗）。
    pub fn set_auto_deny(&self, auto_deny: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.auto_deny = auto_deny;
        }
    }

    fn is_auto_deny(&self) -> bool {
        self.inner.lock().map(|i| i.auto_deny).unwrap_or(false)
    }

    pub fn toggle_yolo(&self) -> PermissionMode {
        if let Ok(mut inner) = self.inner.lock() {
            inner.mode = match inner.mode {
                PermissionMode::Normal => PermissionMode::Yolo,
                PermissionMode::Yolo => PermissionMode::Normal,
            };
            return inner.mode;
        }
        PermissionMode::Normal
    }

    /// 切换到指定会话，从 DB 加载该会话的权限规则
    pub fn switch_session(&self, session_id: String) {
        let storage = {
            let inner = self.inner.lock();
            match inner {
                Ok(inner) => inner.storage.clone(),
                Err(_) => return,
            }
        };
        if let Some(storage) = storage {
            let rules = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    storage
                        .get_session_permission(&session_id)
                        .await
                        .ok()
                        .flatten()
                })
            });
            let loaded = rules
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<SerializableRule>>(json).ok())
                .map(|v| {
                    v.into_iter()
                        .map(|r| PermissionRule {
                            permission: r.permission,
                            pattern: r.pattern,
                            action: r.action,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Ok(mut inner) = self.inner.lock() {
                inner.session_id = Some(session_id);
                inner.session_rules = loaded;
            }
        } else {
            if let Ok(mut inner) = self.inner.lock() {
                inner.session_id = Some(session_id);
                inner.session_rules.clear();
            }
        }
    }

    /// 清除当前会话的内存规则和 session_id（新建会话 / 无会话时调用）
    pub fn clear_session(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.session_id = None;
            inner.session_rules.clear();
        }
    }

    /// 添加会话级规则（用户选择 "always" 后调用）。
    /// 同时持久化到 DB（如果绑定了 storage）。
    pub fn add_session_rules(&self, permission: &str, always_patterns: &[String]) {
        let (session_id, storage) = {
            let inner = self.inner.lock();
            match inner {
                Ok(inner) => (inner.session_id.clone(), inner.storage.clone()),
                Err(_) => return,
            }
        };
        let new_rules: Vec<PermissionRule> = always_patterns
            .iter()
            .map(|pattern| PermissionRule {
                permission: permission.to_string(),
                pattern: pattern.clone(),
                action: PermissionAction::Allow,
            })
            .collect();

        if let Ok(mut inner) = self.inner.lock() {
            // 按 (permission, pattern) 去重：同一规则重复添加（如批量放行
            // 时 TUI 端与 agent 端各 add 一次）不应累积重复项
            for rule in new_rules {
                let dup = inner
                    .session_rules
                    .iter()
                    .any(|r| r.permission == rule.permission && r.pattern == rule.pattern);
                if !dup {
                    inner.session_rules.push(rule);
                }
            }
        }

        // 持久化到 DB
        if let (Some(sid), Some(storage)) = (session_id, storage) {
            let all_rules: Vec<SerializableRule> = {
                let inner = self.inner.lock();
                match inner {
                    Ok(inner) => inner
                        .session_rules
                        .iter()
                        .map(|r| SerializableRule {
                            permission: r.permission.clone(),
                            pattern: r.pattern.clone(),
                            action: r.action,
                        })
                        .collect(),
                    Err(_) => return,
                }
            };
            if let Ok(json) = serde_json::to_string(&all_rules) {
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let _ = storage.set_session_permission(&sid, &json).await;
                    });
                });
            }
        }
    }

    /// 判断工具是否应被禁用（config 规则中该 permission 的最后一条匹配规则为 `*` + deny）。
    /// 用于工具注册层过滤：被禁用的工具对模型完全不可见，无法被会话级 always 绕过。
    /// 语义：只有「最后匹配规则是 * deny」才禁用，
    /// 若存在更具体的规则（如 `git * allow`）则不整体禁用。
    pub fn is_disabled(&self, permission: Option<&str>) -> bool {
        let Some(permission) = permission else {
            return false;
        };
        let config_rules = match self.inner.lock() {
            Ok(inner) => inner.config_rules.clone(),
            Err(_) => return false,
        };
        config_rules
            .iter()
            .rev()
            .find(|r| wildcard_match(permission, &r.permission))
            .map(|r| r.pattern == "*" && r.action == PermissionAction::Deny)
            .unwrap_or(false)
    }

    /// 仅凭现有规则（session + config）判断所有 patterns 是否全部放行。
    /// 不触发弹窗；任一 pattern 未命中 allow 规则则返回 false。
    /// 用于批量放行：用户选择 always 后，检查队列中其他请求是否已被新规则覆盖。
    pub fn rules_allow(&self, permission: &str, patterns: &[String]) -> bool {
        let (config_rules, session_rules) = {
            let inner = match self.inner.lock() {
                Ok(i) => i,
                Err(_) => return false,
            };
            (inner.config_rules.clone(), inner.session_rules.clone())
        };
        patterns.iter().all(|p| {
            evaluate(permission, p, &[&session_rules, &config_rules]) == PermissionAction::Allow
        })
    }

    /// 评估权限：YOLO 模式直接返回 Allow；否则查规则表。
    /// plan_mode 优先级高于 YOLO，开启时强制走权限检查。
    pub fn check(&self, request: &PermissionRequest, plan_mode: bool) -> PermissionResult {
        let mode = self.mode();
        if mode == PermissionMode::Yolo && !plan_mode {
            return PermissionResult::Allowed;
        }

        let (config_rules, session_rules) = {
            let inner = match self.inner.lock() {
                Ok(i) => i,
                Err(_) => return PermissionResult::Allowed,
            };
            (inner.config_rules.clone(), inner.session_rules.clone())
        };
        let defaults = default_rules();

        for pattern in &request.patterns {
            let action = evaluate(
                &request.permission,
                pattern,
                &[&session_rules, &config_rules, &defaults],
            );
            match action {
                PermissionAction::Allow => continue,
                PermissionAction::Deny => {
                    return PermissionResult::Denied(format!(
                        "Permission denied by rule: {} {}",
                        request.permission, pattern
                    ));
                }
                PermissionAction::Ask => {
                    if self.is_auto_deny() {
                        return PermissionResult::Denied(
                            "Permission denied in non-interactive mode (use --yolo to allow all)"
                                .to_string(),
                        );
                    }
                    return PermissionResult::NeedsAsk(request.clone());
                }
            }
        }

        PermissionResult::Allowed
    }
}

/// 内置默认规则（最低优先级，仅在没有命中 session/config 规则时生效）。
/// 工作目录内的操作默认放行，高危场景默认询问：
/// - 全局 `* allow`：放行默认（bash 只读/写命令、目录内 read/edit/write/grep/glob 等）
/// - `external_directory ask`：操作非工作目录内容时询问
/// - `read *.env ask`：读取 .env / .env.local 等敏感文件时询问（.env.example 除外）
/// - `mcp ask`：MCP 工具保持询问（安全敏感，不随全局默认放行）
pub fn default_rules() -> Vec<PermissionRule> {
    vec![
        PermissionRule {
            permission: "*".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Allow,
        },
        PermissionRule {
            permission: "external_directory".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "read".to_string(),
            pattern: "*.env".to_string(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "read".to_string(),
            pattern: "*.env.*".to_string(),
            action: PermissionAction::Ask,
        },
        PermissionRule {
            permission: "read".to_string(),
            pattern: "*.env.example".to_string(),
            action: PermissionAction::Allow,
        },
        PermissionRule {
            permission: "mcp".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Ask,
        },
    ]
}

/// 从多个规则集中查找第一条匹配规则，无匹配时默认 Ask。
/// rulesets 顺序即优先级：越靠前优先级越高。
fn evaluate(permission: &str, pattern: &str, rulesets: &[&[PermissionRule]]) -> PermissionAction {
    for ruleset in rulesets {
        for rule in ruleset.iter().rev() {
            if wildcard_match(permission, &rule.permission)
                && wildcard_match(pattern, &rule.pattern)
            {
                return rule.action;
            }
        }
    }
    PermissionAction::Ask
}

/// 简单通配符匹配：
/// - `\` 统一归一化为 `/`（路径分隔符无关）
/// - Windows 上大小写不敏感
/// - `*` 匹配任意字符序列（含路径分隔符），`?` 匹配单个字符
/// - 其余字符均为字面量（. + ^ $ { } ( ) | [ ] 等不做特殊解释）
/// - 末尾 ` *` 可选（"git *" 同时匹配 "git" 与 "git commit"）
fn wildcard_match(text: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let text = normalize_wildcard(text);
    let pattern = normalize_wildcard(pattern);
    // "git *" / "ls *" 末尾的空格通配符：使其可选
    if let Some(core) = pattern.strip_suffix(" *") {
        return text == core || text.starts_with(&format!("{} ", core));
    }
    glob_like(&text, &pattern)
}

/// 匹配前的归一化：`\` → `/`；Windows 上统一小写。
fn normalize_wildcard(s: &str) -> String {
    #[cfg(windows)]
    {
        s.replace('\\', "/").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s.replace('\\', "/")
    }
}

/// 迭代式 glob 匹配：`*` 匹配任意字符序列（含路径分隔符），`?` 匹配单个字符，
/// 其余字符字面量比较（等价于锚定的 `*` → `.*`、`?` → `.` 正则匹配）。
fn glob_like(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star_t, mut star_p): (Option<usize>, Option<usize>) = (None, None);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_t = Some(ti);
            star_p = Some(pi);
            pi += 1;
        } else if let (Some(st), Some(sp)) = (star_t, star_p) {
            star_t = Some(st + 1);
            ti = st + 1;
            pi = sp + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 从 PermissionConfig (config.toml 中的 [permission.xxx] 表) 构建规则列表
pub fn rules_from_config_table(
    permission_name: &str,
    table: &std::collections::BTreeMap<String, String>,
) -> Vec<PermissionRule> {
    table
        .iter()
        .map(|(pattern, action)| PermissionRule {
            permission: permission_name.to_string(),
            pattern: expand_pattern(pattern),
            action: PermissionAction::from_str(action),
        })
        .collect()
}

fn expand_pattern(pattern: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if pattern == "~" {
            return home_str;
        }
        if pattern.starts_with("~/") {
            return format!("{}{}", home_str, &pattern[1..]);
        }
    }
    pattern.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req() -> PermissionRequest {
        // mcp 权限默认询问（不受全局默认放行影响），适合作为"需要询问"的基准请求
        PermissionRequest {
            permission: "mcp".into(),
            patterns: vec!["example-server".into()],
            always_patterns: vec![],
            description: "test".into(),
        }
    }

    #[test]
    fn test_yolo_mode_allows_everything() {
        let pm = PermissionManager::new(PermissionMode::Yolo, vec![]);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Allowed
        ));
    }

    #[test]
    fn test_yolo_ignored_in_plan_mode() {
        let pm = PermissionManager::new(PermissionMode::Yolo, vec![]);
        assert!(matches!(
            pm.check(&make_req(), true),
            PermissionResult::NeedsAsk(_)
        ));
    }

    #[test]
    fn test_config_allow_rule() {
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "git *".into(),
            action: PermissionAction::Allow,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = PermissionRequest {
            permission: "bash".into(),
            patterns: vec!["git *".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_config_deny_rule() {
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = PermissionRequest {
            permission: "bash".into(),
            patterns: vec!["rm *".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(pm.check(&req, false), PermissionResult::Denied(_)));
    }

    #[test]
    fn test_default_ask() {
        // mcp 默认询问（内置默认规则，不随全局放行）
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::NeedsAsk(_)
        ));
    }

    #[test]
    fn test_default_allow_bash() {
        // 全局默认放行：工作目录内的 bash 命令（含写操作）默认允许
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        for cmd in [
            "git status *",
            "git commit *",
            "rm -rf ./target *",
            "npm install *",
        ] {
            let req = PermissionRequest {
                permission: "bash".into(),
                patterns: vec![cmd.into()],
                always_patterns: vec![],
                description: "test".into(),
            };
            assert!(
                matches!(pm.check(&req, false), PermissionResult::Allowed),
                "expected default allow for {}",
                cmd
            );
        }
    }

    #[test]
    fn test_default_allow_in_workdir_edit() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = PermissionRequest {
            permission: "edit".into(),
            patterns: vec!["C:\\proj\\src\\main.rs".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_default_ask_external_directory() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = PermissionRequest {
            permission: "external_directory".into(),
            patterns: vec!["C:\\proj\\*".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(
            pm.check(&req, false),
            PermissionResult::NeedsAsk(_)
        ));
    }

    #[test]
    fn test_default_read_env_ask() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = |path: &str| PermissionRequest {
            permission: "read".into(),
            patterns: vec![path.into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        // .env / .env.local 默认询问
        assert!(matches!(
            pm.check(&req("C:\\proj\\.env"), false),
            PermissionResult::NeedsAsk(_)
        ));
        assert!(matches!(
            pm.check(&req("C:\\proj\\.env.local"), false),
            PermissionResult::NeedsAsk(_)
        ));
        assert!(matches!(
            pm.check(&req("C:\\proj\\config\\.env.test"), false),
            PermissionResult::NeedsAsk(_)
        ));
        // .env.example 默认放行
        assert!(matches!(
            pm.check(&req("C:\\proj\\.env.example"), false),
            PermissionResult::Allowed
        ));
        // 普通文件默认放行
        assert!(matches!(
            pm.check(&req("C:\\proj\\src\\main.rs"), false),
            PermissionResult::Allowed
        ));
    }

    #[test]
    fn test_config_deny_overrides_default_allow() {
        // 配置规则优先于内置默认放行
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = PermissionRequest {
            permission: "bash".into(),
            patterns: vec!["rm -rf ./target *".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(pm.check(&req, false), PermissionResult::Denied(_)));
    }

    #[test]
    fn test_session_rule_after_always() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("bash", &["git *".to_string()]);
        let req = PermissionRequest {
            permission: "bash".into(),
            patterns: vec!["git *".into()],
            always_patterns: vec![],
            description: "test".into(),
        };
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("git commit", "git *"));
        assert!(wildcard_match("anything", "*"));
        assert!(wildcard_match("cargo build", "cargo build"));
        assert!(!wildcard_match("npm install", "git *"));
        // *.env 规则跨路径分隔符匹配
        assert!(wildcard_match("C:\\proj\\.env", "*.env"));
        assert!(wildcard_match("/home/user/proj/.env.local", "*.env.*"));
        assert!(!wildcard_match("C:\\proj\\.env", "*.env.*"));
        assert!(wildcard_match("C:\\proj\\.env.example", "*.env.example"));
        assert!(!wildcard_match("C:\\proj\\src\\main.rs", "*.env"));
        // 分隔符无关：\ 与 / 等价
        assert!(wildcard_match("C:/proj/file.rs", "C:\\proj\\*"));
        // 正则特殊字符按字面量处理
        assert!(wildcard_match("a.b", "a.b"));
        assert!(!wildcard_match("axb", "a.b"));
        assert!(wildcard_match("file[1].txt", "file[1]*"));
        // 末尾 " *" 可选
        assert!(wildcard_match("git", "git *"));
    }

    #[cfg(windows)]
    #[test]
    fn test_wildcard_match_case_insensitive_on_windows() {
        // Windows 上大小写不敏感
        assert!(wildcard_match("C:\\PROJ\\.ENV", "*.env"));
        assert!(wildcard_match("Git Status *", "git *"));
    }

    #[test]
    fn test_is_disabled_star_deny() {
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        assert!(pm.is_disabled(Some("bash")));
        assert!(!pm.is_disabled(Some("edit")));
        assert!(!pm.is_disabled(None));
    }

    #[test]
    fn test_is_disabled_specific_rule_keeps_tool() {
        // 有更具体的 allow 规则时不整体禁用
        let rules = vec![
            PermissionRule {
                permission: "bash".into(),
                pattern: "*".into(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "bash".into(),
                pattern: "git *".into(),
                action: PermissionAction::Allow,
            },
        ];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        assert!(!pm.is_disabled(Some("bash")));
    }

    #[test]
    fn test_rules_allow() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        assert!(!pm.rules_allow("bash", &["git *".to_string()]));
        pm.add_session_rules("bash", &["git *".to_string()]);
        assert!(pm.rules_allow("bash", &["git commit".to_string()]));
        assert!(!pm.rules_allow("bash", &["rm -rf /".to_string()]));
    }

    #[test]
    fn test_rules_allow_requires_all_patterns() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("bash", &["git *".to_string()]);
        assert!(!pm.rules_allow("bash", &["git commit".to_string(), "rm -rf /".to_string()]));
    }

    #[test]
    fn test_always_flow_auto_approves_same_pattern() {
        // 模拟批量放行语义：请求1 询问 → 用户选 always → 同模式的请求2 自动放行
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = |patterns: Vec<&str>| PermissionRequest {
            permission: "external_directory".into(),
            patterns: patterns.into_iter().map(String::from).collect(),
            always_patterns: vec!["C:\\proj\\*".into()],
            description: "test".into(),
        };
        let first = req(vec!["C:\\proj\\sub\\*"]);
        assert!(matches!(
            pm.check(&first, false),
            PermissionResult::NeedsAsk(_)
        ));
        pm.add_session_rules("external_directory", &first.always_patterns);
        let second = req(vec!["C:\\proj\\other\\*"]);
        assert!(matches!(
            pm.check(&second, false),
            PermissionResult::Allowed
        ));
    }

    #[test]
    fn test_batch_allow_does_not_override_config_deny() {
        // 批量放行判定（rules_allow）不能放行命中 config deny 的请求
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        pm.add_session_rules("bash", &["git *".to_string()]);
        assert!(pm.rules_allow("bash", &["git status *".to_string()]));
        assert!(!pm.rules_allow("bash", &["rm -rf /".to_string()]));
    }

    #[test]
    fn test_add_session_rules_deduplicates() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("bash", &["git *".to_string()]);
        pm.add_session_rules("bash", &["git *".to_string()]);
        let len = {
            let inner = pm.inner.lock().unwrap();
            inner.session_rules.len()
        };
        assert_eq!(len, 1);
    }

    #[test]
    fn test_auto_deny_rejects_ask_instead_of_dialog() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        // 默认：需要询问
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::NeedsAsk(_)
        ));
        // 开启无交互策略：直接拒绝
        pm.set_auto_deny(true);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Denied(_)
        ));
        // 已允许的规则不受影响
        pm.add_session_rules("mcp", &["example-server".to_string()]);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Allowed
        ));
    }

    #[test]
    fn test_auto_deny_yolo_priority() {
        let pm = PermissionManager::new(PermissionMode::Yolo, vec![]);
        pm.set_auto_deny(true);
        // YOLO 优先于 auto_deny：全部放行
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Allowed
        ));
    }
}
