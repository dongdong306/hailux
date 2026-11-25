pub mod shell_arity;
pub mod shell_readonly;
pub mod shell_scan;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Permission action
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

/// Permission mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Default mode: dangerous operations require user confirmation
    Normal,
    /// YOLO mode: skips all permission checks
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

/// A single permission rule
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: PermissionAction,
}

/// Rule struct used for JSON serialization
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

/// Permission evaluation result
#[derive(Debug, Clone)]
pub enum PermissionResult {
    /// Execution allowed
    Allowed,
    /// Denied (with a reason)
    Denied(String),
    /// The user needs to be asked
    NeedsAsk(PermissionRequest),
}

/// Permission request (passed to the TUI dialog)
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// Permission category: "shell", "read", "edit", "write", "mcp", "external_directory"
    pub permission: String,
    /// Matched patterns (used for rule matching)
    pub patterns: Vec<String>,
    /// Patterns persisted when the user picks "always"
    pub always_patterns: Vec<String>,
    /// Short description shown to the user
    pub description: String,
    /// Builtin default rules do not waive this: only explicit session/config
    /// rules may allow or deny; with no explicit hit it falls back to ask.
    /// Used for shell executors (sudo/sh/python etc. — commands whose file
    /// access range cannot be statically determined) — the global `* allow`
    /// is a low-interruption default and must not count as explicit
    /// authorization of arbitrary code execution.
    pub force_ask: bool,
}

/// The user's reply to a permission request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReply {
    /// Allow this once
    Once,
    /// Always allow (similar operations are not asked again this session)
    Always,
    /// Deny
    Deny,
}

struct PermissionInner {
    mode: PermissionMode,
    config_rules: Vec<PermissionRule>,
    session_rules: Vec<PermissionRule>,
    /// Current session ID (for DB persistence)
    session_id: Option<String>,
    /// Storage handle (for persisting session-level rules)
    storage: Option<crate::storage::ChatStorage>,
    /// Non-interactive policy: operations that would ask are auto-denied
    /// instead of raising a dialog.
    /// Used in non-interactive mode (no TUI to answer, avoiding hung tool calls).
    auto_deny: bool,
}

/// Thread-safe permission manager
#[derive(Clone)]
pub struct PermissionManager {
    inner: Arc<Mutex<PermissionInner>>,
}

/// DB rules JSON -> in-memory rules (empty on parse failure; tolerate dirty data)
fn deserialize_session_rules(rules: Option<String>) -> Vec<PermissionRule> {
    rules
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
        .unwrap_or_default()
}

/// Persist session rules (best effort; failures are silent).
async fn persist_session_rules(
    storage: &crate::storage::ChatStorage,
    session_id: &str,
    rules: &[SerializableRule],
) {
    if let Ok(json) = serde_json::to_string(rules) {
        let _ = storage.set_session_permission(session_id, &json).await;
    }
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

    /// Binds the storage handle (&self variant, for binding after construction)
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

    /// Sets the non-interactive policy: when enabled, operations that would
    /// ask are auto-denied (no dialog).
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

    /// Switch to a session, loading its permission rules from the DB (sync
    /// wrapper; async contexts use [`Self::switch_session_async`] to avoid
    /// `block_in_place` panicking on a single-threaded runtime)
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
            let loaded = deserialize_session_rules(rules);
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

    /// Async variant of [`Self::switch_session`] (web path).
    pub async fn switch_session_async(&self, session_id: String) {
        let storage = {
            let inner = self.inner.lock();
            match inner {
                Ok(inner) => inner.storage.clone(),
                Err(_) => return,
            }
        };
        let loaded = match storage {
            Some(storage) => {
                let rules = storage
                    .get_session_permission(&session_id)
                    .await
                    .ok()
                    .flatten();
                deserialize_session_rules(rules)
            }
            None => Vec::new(),
        };
        if let Ok(mut inner) = self.inner.lock() {
            inner.session_id = Some(session_id);
            inner.session_rules = loaded;
        }
    }

    /// Snapshot of the current session's in-memory rules (diagnostics/tests).
    pub fn session_rules(&self) -> Vec<PermissionRule> {
        self.inner
            .lock()
            .map(|i| i.session_rules.clone())
            .unwrap_or_default()
    }

    /// Clears the current session's in-memory rules and session_id
    /// (called when a new session starts / when there is no session)
    pub fn clear_session(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.session_id = None;
            inner.session_rules.clear();
        }
    }

    /// Add session-scoped rules (after the user picks "always"). Also persisted
    /// to the DB when storage is bound. Sync wrapper; async contexts use
    /// [`Self::add_session_rules_async`].
    pub fn add_session_rules(&self, permission: &str, always_patterns: &[String]) {
        let (session_id, storage) = {
            let inner = self.inner.lock();
            match inner {
                Ok(inner) => (inner.session_id.clone(), inner.storage.clone()),
                Err(_) => return,
            }
        };
        let all_rules = self.add_rules_in_memory(permission, always_patterns);
        if let (Some(sid), Some(storage)) = (session_id, storage) {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    persist_session_rules(&storage, &sid, &all_rules).await;
                })
            });
        }
    }

    /// Async variant of [`Self::add_session_rules`] (web path).
    pub async fn add_session_rules_async(&self, permission: &str, always_patterns: &[String]) {
        let (session_id, storage) = {
            let inner = self.inner.lock();
            match inner {
                Ok(inner) => (inner.session_id.clone(), inner.storage.clone()),
                Err(_) => return,
            }
        };
        let all_rules = self.add_rules_in_memory(permission, always_patterns);
        if let (Some(sid), Some(storage)) = (session_id, storage) {
            persist_session_rules(&storage, &sid, &all_rules).await;
        }
    }

    /// In-memory part: merge new rules into session_rules with dedup, returning
    /// the full merged set (for persistence).
    fn add_rules_in_memory(
        &self,
        permission: &str,
        always_patterns: &[String],
    ) -> Vec<SerializableRule> {
        let new_rules: Vec<PermissionRule> = always_patterns
            .iter()
            .map(|pattern| PermissionRule {
                permission: permission.to_string(),
                pattern: pattern.clone(),
                action: PermissionAction::Allow,
            })
            .collect();

        if let Ok(mut inner) = self.inner.lock() {
            // Dedup by (permission, pattern): adding the same rule twice
            // (e.g. batch allow adds once from the TUI side and once from the
            // agent side) must not accumulate duplicates
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

        self.inner
            .lock()
            .map(|inner| {
                inner
                    .session_rules
                    .iter()
                    .map(|r| SerializableRule {
                        permission: r.permission.clone(),
                        pattern: r.pattern.clone(),
                        action: r.action,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a tool should be disabled (in the config rules, the last
    /// matching rule for this permission is `*` + deny).
    /// Used for filtering at tool-registration time: a disabled tool is fully
    /// invisible to the model and cannot be bypassed by a session-level always.
    /// Semantics: disable only when "the last matching rule is * deny";
    /// if a more specific rule exists (e.g. `git * allow`), do not disable wholesale.
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

    /// Decides from existing rules only (session + config) whether all
    /// patterns are allowed.
    /// Never raises a dialog; returns false if any pattern misses an allow rule.
    /// Used for batch allow: after the user picks "always", checks whether
    /// other queued requests are already covered by the new rules.
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

    /// Evaluates permission: YOLO mode returns Allow directly; otherwise the
    /// rule tables are consulted.
    /// plan_mode outranks YOLO: when enabled, permission checks are enforced.
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
            // force_ask: builtin default rules are excluded from evaluation
            // (`* allow` is a low-interruption default, not explicit
            // authorization); only explicit session/config rules may allow or
            // deny; with no explicit hit, evaluate falls back to Ask.
            let action = if request.force_ask {
                evaluate(
                    &request.permission,
                    pattern,
                    &[&session_rules, &config_rules],
                )
            } else {
                evaluate(
                    &request.permission,
                    pattern,
                    &[&session_rules, &config_rules, &defaults],
                )
            };
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

/// Lexically normalizes a path (resolves `.` and `..`) without filesystem access.
/// Used when canonicalize fails (file missing) so that "inside the work dir"
/// can still be judged correctly.
pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Strips the `\\?\` extended path prefix returned by Windows canonicalize so
/// that both canonicalized and lexical fallback paths compare correctly
/// with `starts_with`.
#[cfg(windows)]
fn strip_extended_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

#[cfg(not(windows))]
fn strip_extended_prefix(path: PathBuf) -> PathBuf {
    path
}

/// Resolves a path for permission checks.
/// Relative paths are first joined against work_dir and lexically normalized
/// (resolving ..), then canonicalize is attempted (resolving symlinks etc.);
/// if the file does not exist, the normalized joined path is the fallback.
/// Note: relative paths must not be canonicalized directly — they would
/// resolve against the process cwd instead of work_dir.
pub(crate) fn resolve_permission_path(path: &str, work_dir: &str) -> PathBuf {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        normalize_lexical(&Path::new(work_dir).join(path))
    };
    match std::fs::canonicalize(&joined) {
        Ok(c) => strip_extended_prefix(c),
        Err(_) => {
            // If the file does not exist, walk up to the first existing
            // ancestor directory, canonicalize it, then append the missing
            // parts back. This fixes starts_with mismatches caused by Windows
            // short path names (e.g. RUNNER~1) disagreeing with canonicalize output.
            let mut existing = joined.clone();
            let mut missing: Vec<std::ffi::OsString> = Vec::new();
            while !existing.as_os_str().is_empty() {
                match std::fs::canonicalize(&existing) {
                    Ok(canon) => {
                        let mut result = strip_extended_prefix(canon);
                        for part in missing.into_iter().rev() {
                            result.push(part);
                        }
                        return result;
                    }
                    Err(_) => {
                        if let Some(name) = existing.file_name() {
                            missing.push(name.to_os_string());
                        }
                        if !existing.pop() {
                            break;
                        }
                    }
                }
            }
            strip_extended_prefix(joined)
        }
    }
}

/// Builtin default rules (lowest priority, effective only when no session or
/// config rule matches). In-workdir operations are allowed by default; risky
/// scenarios ask:
/// - Global `* allow`: default allow (shell read/write commands, in-workdir
///   read/edit/write/grep/glob, etc.)
/// - `external_directory ask`: ask when touching content outside the work dir
/// - `read *.env ask`: ask when reading sensitive files like .env / .env.local
///   (.env.example exempt)
/// - `mcp ask`: MCP tools keep asking (security-sensitive, never waived by
///   the global default)
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

/// Whether a file name is an env-sensitive file (same semantics as the
/// builtin read gate rules): matches `*.env` / `*.env.*` but not the
/// exemption rule `*.env.example`.
/// Lets tools that bulk-read content (grep etc.) align with read's .env ask gate.
pub fn is_env_gated_file_name(file_name: &str) -> bool {
    wildcard_match(file_name, "*.env")
        || (wildcard_match(file_name, "*.env.*") && !wildcard_match(file_name, "*.env.example"))
}

/// Finds the first matching rule across multiple rulesets, defaulting to Ask
/// when nothing matches.
/// Ruleset order is priority: earlier rulesets take precedence.
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

/// Simple wildcard matching:
/// - `\` is uniformly normalized to `/` (path-separator agnostic)
/// - Case-insensitive on Windows
/// - `*` matches any character sequence (including path separators); `?`
///   matches a single character
/// - All other characters are literals (. + ^ $ { } ( ) | [ ] etc. get no
///   special treatment)
/// - A trailing ` *` is optional ("git *" matches both "git" and "git commit")
fn wildcard_match(text: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let text = normalize_wildcard(text);
    let pattern = normalize_wildcard(pattern);
    // Trailing space wildcard of "git *" / "ls *": make it optional
    if let Some(core) = pattern.strip_suffix(" *") {
        return text == core || text.starts_with(&format!("{} ", core));
    }
    glob_like(&text, &pattern)
}

/// Normalization before matching: `\` → `/`; lowercased on Windows.
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

/// Iterative glob match: `*` matches any character sequence (including path
/// separators), `?` matches a single character, all other characters compare
/// literally (equivalent to an anchored regex with `*` → `.*` and `?` → `.`).
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

/// Builds a rule list from PermissionConfig (a [permission.xxx] table in config.toml)
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
        // The mcp permission asks by default (unaffected by the global default
        // allow), a good baseline request for "needs ask"
        PermissionRequest {
            permission: "mcp".into(),
            patterns: vec!["example-server".into()],
            always_patterns: vec![],
            description: "test".into(),
            force_ask: false,
        }
    }

    fn test_req(permission: &str, patterns: &[&str]) -> PermissionRequest {
        PermissionRequest {
            permission: permission.into(),
            patterns: patterns.iter().map(|s| (*s).into()).collect(),
            always_patterns: vec![],
            description: "test".into(),
            force_ask: false,
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
            permission: "shell".into(),
            pattern: "git *".into(),
            action: PermissionAction::Allow,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = test_req("shell", &["git *"]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_config_deny_rule() {
        let rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = test_req("shell", &["rm *"]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Denied(_)));
    }

    #[test]
    fn test_default_ask() {
        // mcp asks by default (builtin default rule, not waived by the global allow)
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::NeedsAsk(_)
        ));
    }

    #[test]
    fn test_default_allow_shell() {
        // Global default allow: in-workdir shell commands (incl. writes) allowed by default
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        for cmd in [
            "git status *",
            "git commit *",
            "rm -rf ./target *",
            "npm install *",
        ] {
            let req = test_req("shell", &[cmd]);
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
        let req = test_req("edit", &["C:\\proj\\src\\main.rs"]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_default_ask_external_directory() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = test_req("external_directory", &["C:\\proj\\*"]);
        assert!(matches!(
            pm.check(&req, false),
            PermissionResult::NeedsAsk(_)
        ));
    }

    #[test]
    fn test_default_read_env_ask() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = |path: &str| test_req("read", &[path]);
        // .env / .env.local ask by default
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
        // .env.example is allowed by default
        assert!(matches!(
            pm.check(&req("C:\\proj\\.env.example"), false),
            PermissionResult::Allowed
        ));
        // Ordinary files are allowed by default
        assert!(matches!(
            pm.check(&req("C:\\proj\\src\\main.rs"), false),
            PermissionResult::Allowed
        ));
    }

    #[test]
    fn test_env_gated_file_name() {
        // Same semantics as the builtin read rules: *.env / *.env.* hit,
        // .env.example exempt
        assert!(is_env_gated_file_name(".env"));
        assert!(is_env_gated_file_name("k.env"));
        assert!(is_env_gated_file_name(".env.local"));
        assert!(is_env_gated_file_name("prod.env.json"));
        assert!(!is_env_gated_file_name(".env.example"));
        assert!(!is_env_gated_file_name("example.env.example"));
        assert!(!is_env_gated_file_name("main.rs"));
        assert!(!is_env_gated_file_name("env.txt"));
    }

    #[test]
    fn test_config_deny_overrides_default_allow() {
        // Config rules take precedence over the builtin default allow
        let rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        let req = test_req("shell", &["rm -rf ./target *"]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Denied(_)));
    }

    #[test]
    fn test_session_rule_after_always() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("shell", &["git *".to_string()]);
        let req = test_req("shell", &["git *"]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_force_ask_ignores_builtin_defaults() {
        // The global default `* allow` does not waive force_ask requests:
        // no explicit rule → ask
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let mut req = test_req("shell", &["sudo *"]);
        req.force_ask = true;
        assert!(matches!(
            pm.check(&req, false),
            PermissionResult::NeedsAsk(_)
        ));
        // After the user picks "always" (explicit session rule) → allowed,
        // no more nagging
        pm.add_session_rules("shell", &["sudo *".to_string()]);
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_force_ask_respects_config_rules() {
        let mut req = test_req("shell", &["sudo *"]);
        req.force_ask = true;
        // Explicit config allow: permitted
        let allow_rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "sudo *".into(),
            action: PermissionAction::Allow,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, allow_rules);
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
        // Explicit config deny: denial takes precedence
        let deny_rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "sudo *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, deny_rules);
        assert!(matches!(pm.check(&req, false), PermissionResult::Denied(_)));
    }

    #[test]
    fn test_force_ask_still_allowed_in_yolo() {
        // YOLO semantics = skip all permission confirmations; force_ask is no exception
        let pm = PermissionManager::new(PermissionMode::Yolo, vec![]);
        let mut req = test_req("shell", &["sudo *"]);
        req.force_ask = true;
        assert!(matches!(pm.check(&req, false), PermissionResult::Allowed));
    }

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("git commit", "git *"));
        assert!(wildcard_match("anything", "*"));
        assert!(wildcard_match("cargo build", "cargo build"));
        assert!(!wildcard_match("npm install", "git *"));
        // *.env rules match across path separators
        assert!(wildcard_match("C:\\proj\\.env", "*.env"));
        assert!(wildcard_match("/home/user/proj/.env.local", "*.env.*"));
        assert!(!wildcard_match("C:\\proj\\.env", "*.env.*"));
        assert!(wildcard_match("C:\\proj\\.env.example", "*.env.example"));
        assert!(!wildcard_match("C:\\proj\\src\\main.rs", "*.env"));
        // Separator agnostic: \ and / are equivalent
        assert!(wildcard_match("C:/proj/file.rs", "C:\\proj\\*"));
        // Regex special characters are treated as literals
        assert!(wildcard_match("a.b", "a.b"));
        assert!(!wildcard_match("axb", "a.b"));
        assert!(wildcard_match("file[1].txt", "file[1]*"));
        // Trailing " *" is optional
        assert!(wildcard_match("git", "git *"));
    }

    #[cfg(windows)]
    #[test]
    fn test_wildcard_match_case_insensitive_on_windows() {
        // Case-insensitive on Windows
        assert!(wildcard_match("C:\\PROJ\\.ENV", "*.env"));
        assert!(wildcard_match("Git Status *", "git *"));
    }

    #[test]
    fn test_is_disabled_star_deny() {
        let rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        assert!(pm.is_disabled(Some("shell")));
        assert!(!pm.is_disabled(Some("edit")));
        assert!(!pm.is_disabled(None));
    }

    #[test]
    fn test_is_disabled_specific_rule_keeps_tool() {
        // Not disabled wholesale when a more specific allow rule exists
        let rules = vec![
            PermissionRule {
                permission: "shell".into(),
                pattern: "*".into(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "shell".into(),
                pattern: "git *".into(),
                action: PermissionAction::Allow,
            },
        ];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        assert!(!pm.is_disabled(Some("shell")));
    }

    #[test]
    fn test_rules_allow() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        assert!(!pm.rules_allow("shell", &["git *".to_string()]));
        pm.add_session_rules("shell", &["git *".to_string()]);
        assert!(pm.rules_allow("shell", &["git commit".to_string()]));
        assert!(!pm.rules_allow("shell", &["rm -rf /".to_string()]));
    }

    #[test]
    fn test_rules_allow_requires_all_patterns() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("shell", &["git *".to_string()]);
        assert!(!pm.rules_allow("shell", &["git commit".to_string(), "rm -rf /".to_string()]));
    }

    #[test]
    fn test_always_flow_auto_approves_same_pattern() {
        // Simulates batch-allow semantics: request 1 asks → user picks
        // "always" → request 2 with the same pattern is auto-allowed
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        let req = |patterns: Vec<&str>| PermissionRequest {
            permission: "external_directory".into(),
            patterns: patterns.into_iter().map(String::from).collect(),
            always_patterns: vec!["C:\\proj\\*".into()],
            description: "test".into(),
            force_ask: false,
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
        // Batch-allow decision (rules_allow) must not allow requests hitting a config deny
        let rules = vec![PermissionRule {
            permission: "shell".into(),
            pattern: "rm *".into(),
            action: PermissionAction::Deny,
        }];
        let pm = PermissionManager::new(PermissionMode::Normal, rules);
        pm.add_session_rules("shell", &["git *".to_string()]);
        assert!(pm.rules_allow("shell", &["git status *".to_string()]));
        assert!(!pm.rules_allow("shell", &["rm -rf /".to_string()]));
    }

    #[test]
    fn test_add_session_rules_deduplicates() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        pm.add_session_rules("shell", &["git *".to_string()]);
        pm.add_session_rules("shell", &["git *".to_string()]);
        let len = {
            let inner = pm.inner.lock().unwrap();
            inner.session_rules.len()
        };
        assert_eq!(len, 1);
    }

    #[test]
    fn test_auto_deny_rejects_ask_instead_of_dialog() {
        let pm = PermissionManager::new(PermissionMode::Normal, vec![]);
        // Default: needs ask
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::NeedsAsk(_)
        ));
        // Non-interactive policy enabled: denied directly
        pm.set_auto_deny(true);
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Denied(_)
        ));
        // Already-allowed rules are unaffected
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
        // YOLO outranks auto_deny: everything allowed
        assert!(matches!(
            pm.check(&make_req(), false),
            PermissionResult::Allowed
        ));
    }
}
