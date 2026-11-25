//! bash 命令静态安全扫描。
//!
//! bash 本质是任意代码执行，本扫描是尽力而为的打扰控制而非安全边界：
//! - 识别「执行原语」（sudo / sh / python / eval / ...）：无法静态判定其
//!   文件访问范围，调用方应将 bash 权限请求标记为 force_ask；
//! - 识别「文件操作命令」的路径参数与任意命令的重定向目标，解析出工作
//!   目录之外的访问目标，供 external_directory 权限请求使用；
//! - 无法展开的动态表达式（`$var` / `$(...)` / 反引号）被跳过——这是有意
//!   的权衡：已知变量（`$HOME` / `$env:...`）会尝试展开，未知变量无法
//!   判定，宁可漏报也不把界内常用写法误报成弹窗。

use std::path::{Path, PathBuf};

use super::resolve_permission_path;

/// bash 中带文件路径语义的命令：全部参数都按路径候选扫描
/// （标志位 `-x` / `+x` 除外）。含 POSIX、PowerShell 全名与高频短别名。
/// `cd` 不在此列：它由 scan 的基准跟踪机制单独处理。
const FILE_COMMANDS: &[&str] = &[
    "cat",
    "rm",
    "cp",
    "mv",
    "mkdir",
    "touch",
    "chmod",
    "chown",
    "ln",
    "tee",
    "find",
    "grep",
    "sed",
    "head",
    "tail",
    "install",
    "rsync",
    "scp",
    "patch",
    "type",
    // PowerShell 全名
    "get-content",
    "set-content",
    "add-content",
    "copy-item",
    "move-item",
    "remove-item",
    "new-item",
    "rename-item",
    "out-file",
    // PowerShell 高频短别名
    "gc",
    "gci",
    "sc",
    "ac",
    "si",
    "sp",
    "ni",
    "ri",
    "cpi",
    "mi",
    "rni",
];

/// 执行原语：第一个 token 命中即视为「无法静态分析」。
/// 这些命令可以执行任意代码 / 访问任意路径，词法扫描无法给出可靠边界。
const EXECUTOR_COMMANDS: &[&str] = &[
    // 提权 / 派生 shell
    "sudo",
    "doas",
    "su",
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "pwsh",
    "powershell",
    "powershell.exe",
    "pwsh.exe",
    "cmd",
    "cmd.exe",
    // 解释器
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "tsx",
    "ts-node",
    "ruby",
    "perl",
    "php",
    "lua",
    "rscript",
    "julia",
    "awk",
    "gawk",
    // 加载 / 求值 / 包装执行
    "eval",
    "exec",
    "source",
    ".",
    "xargs",
    "env",
    "nohup",
    "setsid",
    "stdbuf",
    "timeout",
    "watch",
    "script",
    "expect",
    // PowerShell 执行原语
    "start",
    "start-process",
    "invoke-expression",
    "iex",
    "&",
];

/// 单条命令的扫描结论。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BashScan {
    /// 命中执行原语：bash 权限请求需标记 force_ask（内置默认放行不豁免）
    pub executor_hit: bool,
    /// 命中的越界目录（已去重排序），供 external_directory 请求构造 patterns
    pub external_dirs: Vec<PathBuf>,
}

impl BashScan {
    pub fn is_clean(&self) -> bool {
        !self.executor_hit && self.external_dirs.is_empty()
    }
}

/// 扫描完整命令字符串（可能含多个子命令），汇总所有子命令的结论。
///
/// `cd X` 会更新后续子命令的路径解析基准（`base_dir`）：拦截点落在
/// 真正访问文件的段上——`cd .. && ls` 无文件访问语义，不打扰；而
/// `cd /etc && cat shadow` 由 `cat shadow` 按 `/etc` 基准解析后命中越界，
/// always 的授权面也缩小到实际访问的具体目录。
pub fn scan(command: &str, work_dir: &str) -> BashScan {
    let work_canonical = resolve_permission_path(work_dir, work_dir);
    let mut base_dir = work_dir.to_string();
    let mut result = BashScan::default();
    for seg in split_top_level(command) {
        scan_segment(seg, &base_dir, &work_canonical, &mut result);
        if let Some(target) = cd_target(seg)
            && let Some(expanded) = expand_candidate(&target)
        {
            base_dir = resolve_permission_path(&expanded, &base_dir)
                .display()
                .to_string();
        }
    }
    result.external_dirs.sort();
    result.external_dirs.dedup();
    result
}

/// 命令分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmdKind {
    /// 文件操作命令：扫描全部参数（含重定向目标）
    FileOps,
    /// 执行原语：无法静态分析，记 force_ask
    Executor,
    /// 其余命令：仅扫描重定向目标（`> /etc/x` 对任何命令都是路径写）
    Other,
}

fn classify(cmd: &str) -> CmdKind {
    let cmd = cmd.to_ascii_lowercase();
    if FILE_COMMANDS.contains(&cmd.as_str()) {
        CmdKind::FileOps
    } else if EXECUTOR_COMMANDS.contains(&cmd.as_str()) {
        CmdKind::Executor
    } else {
        CmdKind::Other
    }
}

fn scan_segment(seg: &str, base_dir: &str, work_canonical: &Path, result: &mut BashScan) {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return;
    };
    let cmd = first.trim_matches(['"', '\'']);
    match classify(cmd) {
        CmdKind::Executor => result.executor_hit = true,
        CmdKind::FileOps => scan_tokens(&tokens[1..], base_dir, work_canonical, result, true),
        CmdKind::Other => scan_tokens(&tokens, base_dir, work_canonical, result, false),
    }
}

/// 扫描一段命令的 token 序列。
///
/// - `include_plain_args`：是否把普通参数当路径候选（FileOps 为 true）；
///   重定向目标对任何命令类型都检查。
/// - 重定向运算符可独立成 token（`echo x > /etc/y`）或与路径粘连
///   （`echo x 2>/dev/null`），两种形态都处理。
fn scan_tokens(
    tokens: &[&str],
    base_dir: &str,
    work_canonical: &Path,
    result: &mut BashScan,
    include_plain_args: bool,
) {
    let mut redirect_next = false;
    for token in tokens {
        if redirect_next {
            redirect_next = false;
            check_candidate(token, base_dir, work_canonical, result);
            continue;
        }
        if let Some(rest) = strip_redirect_operator(token) {
            if rest.is_empty() {
                // 独立运算符 token：目标在下一个 token
                redirect_next = true;
            } else {
                check_candidate(rest, base_dir, work_canonical, result);
            }
            continue;
        }
        if !include_plain_args {
            continue;
        }
        let arg = token.trim_matches(['"', '\'']);
        if arg.starts_with('-') || arg.starts_with('+') {
            continue;
        }
        check_candidate(arg, base_dir, work_canonical, result);
    }
}

/// 提取 `cd` 的目标参数（用于更新后续子命令的路径解析基准）。
/// 无目标 / `-`（上一个目录，无法静态判定）/ 选项开头 → None。
fn cd_target(seg: &str) -> Option<String> {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let first = tokens.first()?.trim_matches(['"', '\'']);
    if !first.eq_ignore_ascii_case("cd") {
        return None;
    }
    let raw = tokens.get(1)?.trim_matches(['"', '\'']);
    if raw.starts_with('-') {
        return None;
    }
    Some(raw.to_string())
}

/// 展开路径候选中的 `~` 与已知变量引用；无法静态展开的动态表达式
/// （未知 `$var` / `$(...)` / 反引号）返回 None。
fn expand_candidate(arg: &str) -> Option<String> {
    if arg == "~" {
        return dirs::home_dir().map(|h| h.display().to_string());
    }
    if let Some(rest) = arg.strip_prefix("~/").or_else(|| arg.strip_prefix("~\\")) {
        return dirs::home_dir().map(|h| format!("{}\\{}", h.display(), rest));
    }
    if arg.contains(['$', '(', '`']) {
        return expand_known_vars(arg);
    }
    Some(arg.to_string())
}

/// 检查单个路径候选：展开 `~` 与已知变量后，按当前解析基准判断是否越界。
/// 无法静态展开的动态表达式跳过（尽力而为）。
fn check_candidate(raw: &str, base_dir: &str, work_canonical: &Path, result: &mut BashScan) {
    let arg = raw.trim_matches(['"', '\'']);
    if arg.is_empty() {
        return;
    }
    let Some(expanded) = expand_candidate(arg) else {
        return;
    };
    let resolved = resolve_permission_path(&expanded, base_dir);
    if resolved.starts_with(work_canonical) {
        return;
    }
    let dir = if resolved.is_dir() {
        resolved
    } else {
        resolved
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(resolved)
    };
    result.external_dirs.push(dir);
}

/// 在引号与转义之外按命令分隔符（`;` `|` `&&` `||` `&` 换行）切分子命令。
/// 引号内的分隔符不切分；连续分隔符产生的空段被丢弃。
fn split_top_level(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, c) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !in_single => escaped = true,
            // PowerShell 反引号转义（双引号内亦有效，粗略同样处理）
            '`' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' | '&' | '|' | '\n' if !in_single && !in_double => {
                segments.push(&command[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    segments.push(&command[start..]);
    segments.retain(|s| !s.trim().is_empty());
    segments
}

/// 若 token 以重定向运算符开头，返回运算符之后的剩余部分；
/// 独立运算符 token 返回空串。非重定向 token 返回 None。
/// 支持形态：`>f` `>>f` `>|f` `2>f` `2>>f` `&>f` `<f`；
/// `<<EOF`（heredoc 定界符）与 `2>&1`（fd 复制）不视为路径。
fn strip_redirect_operator(token: &str) -> Option<&str> {
    let bytes = token.as_bytes();
    let mut idx = 0usize;
    // 前导 fd 说明符：`2>` / `1>` / `&>`
    if idx + 1 < bytes.len()
        && (bytes[idx] == b'1' || bytes[idx] == b'2' || bytes[idx] == b'&')
        && bytes[idx + 1] == b'>'
    {
        idx += 1;
    }
    match bytes.get(idx) {
        Some(b'<') => {
            // `<<word` 是 heredoc 定界符，非路径
            if bytes.get(idx + 1) == Some(&b'<') {
                return None;
            }
            idx += 1;
        }
        Some(b'>') => {
            idx += 1;
            // `>>` 追加 / `>|` 强制覆盖
            if matches!(bytes.get(idx), Some(b'>') | Some(b'|')) {
                idx += 1;
            }
        }
        _ => return None,
    }
    let rest = &token[idx..];
    // `2>&1`：目标以 `&` 开头是 fd 复制，非路径
    if rest.starts_with('&') {
        return None;
    }
    Some(rest)
}

/// 展开参数中的已知变量引用。支持 `$NAME` / `${NAME}` / `$env:NAME`
/// （PowerShell 语法，`env:` 大小写不敏感）。仅当参数中恰好只有一个变量
/// 引用、其余部分为字面量时展开成功；无法解析时返回 None（调用方跳过）。
fn expand_known_vars(arg: &str) -> Option<String> {
    let dollar = arg.find('$')?;
    let (value, after) = parse_var_ref(&arg[dollar..])?;
    // 变量引用之后不允许再有动态表达式
    if after.contains(['$', '`', '(']) {
        return None;
    }
    Some(format!("{}{}{}", &arg[..dollar], value, after))
}

/// 解析以 `$` 开头的变量引用，返回（值, 引用之后的剩余部分）。
fn parse_var_ref(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix("${") {
        let end = rest.find('}')?;
        let value = lookup_env(&rest[..end])?;
        return Some((value, &rest[end + 1..]));
    }
    let body = s.strip_prefix('$')?;
    // `$env:NAME`（PowerShell）优先于普通标识符
    let named = if body.len() >= 4 && body[..4].eq_ignore_ascii_case("env:") {
        take_ident(&body[4..])
    } else {
        take_ident(body)
    };
    if named.0.is_empty() {
        return None;
    }
    Some((lookup_env(&named.0)?, named.1))
}

fn take_ident(s: &str) -> (String, &str) {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (s[..end].to_string(), &s[end..])
}

fn lookup_env(name: &str) -> Option<String> {
    match name.to_ascii_uppercase().as_str() {
        "HOME" | "USERPROFILE" => dirs::home_dir().map(|h| h.display().to_string()),
        _ => std::env::var(name).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_workdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hailux_bash_scan_{}", name));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn inside_workdir_is_clean() {
        let wd = tmp_workdir("inside");
        let wd_str = wd.display().to_string();
        for cmd in [
            "ls -la",
            "git status",
            "cat src/main.rs",
            "rm -rf ./target",
            "mkdir -p build",
            "npm install",
            "curl https://example.com",
            "grep -r pattern .",
            "sed -i 's/a/b/' file.txt",
            "ln -s target link",
            "tee out.log",
            "cat -n a.txt b.txt",
        ] {
            let result = scan(cmd, &wd_str);
            assert!(
                result.is_clean(),
                "expected clean for {cmd:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn external_paths_are_detected() {
        let wd = tmp_workdir("outside");
        let wd_str = wd.display().to_string();

        let result = scan("cat ../secrets.txt", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);

        let result = scan("cat /etc/hosts", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);

        // 多个子命令逐段扫描：cd 跟踪解析基准，拦截点在真正访问文件的段
        let result = scan("cd /etc && cat shadow", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // 界内 cd 与无文件访问的后续命令不打扰
        assert!(scan("cd build && ls", &wd_str).is_clean());
        assert!(scan("cd .. && ls", &wd_str).is_clean());
        assert!(
            scan("cd /etc", &wd_str).is_clean(),
            "bare cd is a no-op for the spawned shell"
        );

        // 多个越界目标一次收齐（同目录去重）
        let result = scan("cp /etc/hosts /etc/passwd .", &wd_str);
        assert_eq!(result.external_dirs.len(), 1, "same dir dedup: {result:?}");

        // 管道右侧同样扫描
        let result = scan("curl https://x | tee /etc/hosts", &wd_str);
        assert!(!result.external_dirs.is_empty());

        // PowerShell 短别名与补齐的路径命令同样受检
        // （Windows 盘符路径仅在 Windows 上是"外部路径"，Linux 下反斜杠
        // 属于文件名字符，会被解析为界内相对路径）
        #[cfg(windows)]
        for cmd in [
            "gc C:\\Windows\\win.ini",
            "gci C:\\Windows",
            "ni C:\\Users\\evil.txt",
        ] {
            let result = scan(cmd, &wd_str);
            assert!(
                !result.external_dirs.is_empty(),
                "expected external hit for {cmd:?}"
            );
        }
        for cmd in [
            "install -m 644 /etc/hosts .",
            "rsync -a /etc/ .",
            "scp /etc/hosts backup/",
        ] {
            let result = scan(cmd, &wd_str);
            assert!(
                !result.external_dirs.is_empty(),
                "expected external hit for {cmd:?}"
            );
        }
        // 界内使用短别名不误报
        assert!(scan("gc notes.txt", &wd_str).is_clean());
        assert!(scan("ni -ItemType Directory sub", &wd_str).is_clean());
    }

    #[test]
    fn executors_force_ask() {
        let wd = tmp_workdir("executor");
        let wd_str = wd.display().to_string();
        for cmd in [
            "sudo systemctl restart nginx",
            "sh -c 'echo hi'",
            "python script.py",
            "node -e 'console.log(1)'",
            "eval $(echo hi)",
            "xargs rm list.txt",
            "powershell -c \"Get-Content notes.txt\"",
            "source .env",
        ] {
            let result = scan(cmd, &wd_str);
            assert!(result.executor_hit, "expected executor hit for {cmd:?}");
        }
        // 界内路径不产生越界命中
        let result = scan("python script.py", &wd_str);
        assert!(result.external_dirs.is_empty());
    }

    #[test]
    fn redirect_targets_checked_for_any_command() {
        let wd = tmp_workdir("redirect");
        let wd_str = wd.display().to_string();
        // echo 不属于文件命令，但重定向目标必须检查
        let result = scan("echo hi > /etc/hosts", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // 运算符与路径粘连
        let result = scan("echo hi 2>/etc/error.log", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // fd 复制与 heredoc 不是路径
        assert!(scan("echo hi 2>&1", &wd_str).is_clean());
        assert!(scan("cat <<EOF", &wd_str).is_clean());
        // 界内重定向不误报
        assert!(scan("echo hi > out.txt", &wd_str).is_clean());
    }

    #[test]
    fn known_vars_are_expanded() {
        let wd = tmp_workdir("vars");
        let wd_str = wd.display().to_string();
        // $HOME / $env: 指向家目录 → 必然越界
        let result = scan("cat $HOME/.ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        let result = scan("cat ~/.ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        #[cfg(windows)]
        {
            let result = scan("get-content $env:USERPROFILE\\.ssh\\id_rsa", &wd_str);
            assert_eq!(result.external_dirs.len(), 1);
        }
        // 未知变量无法展开 → 跳过（尽力而为）
        assert!(scan("cat $UNKNOW_VAR/file", &wd_str).is_clean());
        // 命令替换跳过
        assert!(scan("cat $(pwd)/file", &wd_str).is_clean());
    }

    #[test]
    fn glob_parent_dir_still_gated() {
        let wd = tmp_workdir("glob");
        let wd_str = wd.display().to_string();
        // 界内 glob 放行
        assert!(scan("rm build/*.o", &wd_str).is_clean());
        assert!(scan("cp src/*.rs backup/", &wd_str).is_clean());
        // 越界 glob：`..` 前缀明确指向外部，仍被拦截
        assert_eq!(scan("cat ../secrets/*.pem", &wd_str).external_dirs.len(), 1);
    }

    #[test]
    fn cd_updates_resolution_base() {
        let wd = tmp_workdir("cd_base");
        let wd_str = wd.display().to_string();
        // cd 出界后的相对路径访问按新基准解析 → 命中，目录为实际访问位置。
        // 期望值同样经 resolve_permission_path 规范化：CI 的 TEMP 环境变量
        // 可能含 8.3 短名（RUNNER~1），canonicalize 后是长名，两边必须同构。
        let result = scan("cd .. && cat secret.txt", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        let expected_parent =
            resolve_permission_path(wd.parent().unwrap().to_string_lossy().as_ref(), &wd_str);
        assert_eq!(
            result.external_dirs[0], expected_parent,
            "hit dir must be the cd target, not a blanket out-of-workdir mark"
        );
        // 链式 cd：基准逐段更新
        let result = scan("cd ../.. && cat x", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // cd 到家目录后读家目录下的敏感文件 → 命中
        let result = scan("cd ~ && cat .ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // cd 界内子目录后访问仍安全
        assert!(scan("cd build && cat out.txt", &wd_str).is_clean());
        // cd - 与选项无法静态判定 → 基准保持，不误判
        assert!(scan("cd - && ls", &wd_str).is_clean());
    }

    #[test]
    fn quoted_separators_are_not_split() {
        assert_eq!(split_top_level("echo 'a && b' && cat /etc/hosts").len(), 2);
        assert_eq!(split_top_level("echo \"x; y\" ; ls").len(), 2);
        assert_eq!(split_top_level("echo 'it''s fine'").len(), 1);
        assert_eq!(split_top_level("printf '%s\\n' 'a|b'").len(), 1);
        assert!(split_top_level("").is_empty());
        assert!(split_top_level("   ").is_empty());
    }

    #[test]
    fn classify_covers_both_shells() {
        assert_eq!(classify("cat"), CmdKind::FileOps);
        assert_eq!(classify("Get-Content"), CmdKind::FileOps);
        assert_eq!(classify("git"), CmdKind::Other);
        assert_eq!(classify("sudo"), CmdKind::Executor);
        assert_eq!(classify("PYTHON3"), CmdKind::Executor);
    }
}
