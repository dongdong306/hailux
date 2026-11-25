//! Best-effort static scan of shell commands for permission gating.
//!
//! A shell command is arbitrary code execution, so this scan is interruption
//! control, not a security boundary:
//! - Executor commands (sudo / sh / python / eval / ...) have unknowable file
//!   access; callers must mark the shell permission request `force_ask`;
//! - Path arguments of file-oriented commands and redirect targets of any
//!   command are resolved to detect accesses outside the work directory,
//!   feeding the `external_directory` permission request;
//! - Dynamic expressions that cannot be expanded (`$var` / `$(...)` /
//!   backticks) are skipped on purpose: known variables (`$HOME` /
//!   `$env:...`) are expanded, unknown ones are unknowable — prefer false
//!   negatives over nagging on common in-workdir usage.

use std::path::{Path, PathBuf};

use super::resolve_permission_path;

/// Shell commands with file-path semantics: all arguments are scanned as path
/// candidates (except flags like `-x` / `+x`). Covers POSIX and PowerShell
/// full names plus frequent short aliases. `cd` is not listed: it is handled
/// separately by scan's base-directory tracking.
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
    // PowerShell full names
    "get-content",
    "set-content",
    "add-content",
    "copy-item",
    "move-item",
    "remove-item",
    "new-item",
    "rename-item",
    "out-file",
    // Frequent PowerShell short aliases
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

/// Execution primitives: a first-token hit means "not statically analyzable".
/// These commands can execute arbitrary code / access arbitrary paths; a
/// lexical scan cannot provide a reliable boundary.
const EXECUTOR_COMMANDS: &[&str] = &[
    // Privilege escalation / derived shells
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
    // Interpreters
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
    // Load / evaluate / wrapped execution
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
    // PowerShell execution primitives
    "start",
    "start-process",
    "invoke-expression",
    "iex",
    "&",
];

/// Scan result for a single command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShellScan {
    /// Executor hit: the shell permission request must be marked `force_ask`
    /// (builtin default allow does not waive it)
    pub executor_hit: bool,
    /// Hit out-of-workdir directories (deduped and sorted); used to build
    /// patterns for the external_directory request
    pub external_dirs: Vec<PathBuf>,
}

impl ShellScan {
    pub fn is_clean(&self) -> bool {
        !self.executor_hit && self.external_dirs.is_empty()
    }
}

/// Scans a full command string (possibly containing multiple subcommands) and
/// aggregates the results of all subcommands.
///
/// `cd X` updates the path resolution base (`base_dir`) for subsequent
/// subcommands: the interception point lands on the segment that actually
/// accesses files — `cd .. && ls` has no file-access semantics and does not
/// interrupt, while `cd /etc && cat shadow` is resolved by `cat shadow`
/// against the `/etc` base and hits as out-of-bounds; the "always" grant
/// scope also shrinks to the specific directory actually accessed.
pub fn scan(command: &str, work_dir: &str) -> ShellScan {
    let work_canonical = resolve_permission_path(work_dir, work_dir);
    let mut base_dir = work_dir.to_string();
    let mut result = ShellScan::default();
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

/// Command classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmdKind {
    /// File-operation command: all arguments scanned (including redirect targets)
    FileOps,
    /// Execution primitive: not statically analyzable, marks force_ask
    Executor,
    /// Other commands: only redirect targets scanned (`> /etc/x` is a path
    /// write for any command)
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

fn scan_segment(seg: &str, base_dir: &str, work_canonical: &Path, result: &mut ShellScan) {
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

/// Scans the token sequence of one command segment.
///
/// - `include_plain_args`: whether plain arguments count as path candidates
///   (true for FileOps); redirect targets are checked for any command kind.
/// - A redirect operator can be a standalone token (`echo x > /etc/y`) or
///   glued to the path (`echo x 2>/dev/null`); both forms are handled.
fn scan_tokens(
    tokens: &[&str],
    base_dir: &str,
    work_canonical: &Path,
    result: &mut ShellScan,
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
                // Standalone operator token: target is the next token
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

/// Extracts the target argument of `cd` (used to update the path resolution
/// base for subsequent subcommands).
/// No target / `-` (previous directory, not statically decidable) / option
/// prefix → None.
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

/// Expands `~` and known variable references in a path candidate; dynamic
/// expressions that cannot be statically expanded (unknown `$var` / `$(...)`
/// / backticks) return None.
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

/// Checks a single path candidate: after expanding `~` and known variables,
/// decides against the current resolution base whether it is out-of-bounds.
/// Dynamic expressions that cannot be statically expanded are skipped
/// (best effort).
fn check_candidate(raw: &str, base_dir: &str, work_canonical: &Path, result: &mut ShellScan) {
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

/// Splits into subcommands by command separators (`;` `|` `&&` `||` `&`
/// newline) outside quotes and escapes.
/// Separators inside quotes do not split; empty segments produced by
/// consecutive separators are dropped.
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
            // PowerShell backtick escape (also valid inside double quotes;
            // handled the same way, roughly)
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

/// If the token starts with a redirect operator, returns the remainder after
/// the operator; a standalone operator token returns an empty string.
/// Non-redirect tokens return None.
/// Supported forms: `>f` `>>f` `>|f` `2>f` `2>>f` `&>f` `<f`;
/// `<<EOF` (heredoc delimiter) and `2>&1` (fd duplication) are not paths.
fn strip_redirect_operator(token: &str) -> Option<&str> {
    let bytes = token.as_bytes();
    let mut idx = 0usize;
    // Leading fd specifier: `2>` / `1>` / `&>`
    if idx + 1 < bytes.len()
        && (bytes[idx] == b'1' || bytes[idx] == b'2' || bytes[idx] == b'&')
        && bytes[idx + 1] == b'>'
    {
        idx += 1;
    }
    match bytes.get(idx) {
        Some(b'<') => {
            // `<<word` is a heredoc delimiter, not a path
            if bytes.get(idx + 1) == Some(&b'<') {
                return None;
            }
            idx += 1;
        }
        Some(b'>') => {
            idx += 1;
            // `>>` append / `>|` forced overwrite
            if matches!(bytes.get(idx), Some(b'>') | Some(b'|')) {
                idx += 1;
            }
        }
        _ => return None,
    }
    let rest = &token[idx..];
    // `2>&1`: a target starting with `&` is fd duplication, not a path
    if rest.starts_with('&') {
        return None;
    }
    Some(rest)
}

/// Expands known variable references in an argument. Supports `$NAME` /
/// `${NAME}` / `$env:NAME` (PowerShell syntax, `env:` case-insensitive).
/// Expansion succeeds only when the argument contains exactly one variable
/// reference and the rest is literal; unresolvable cases return None
/// (the caller skips them).
fn expand_known_vars(arg: &str) -> Option<String> {
    let dollar = arg.find('$')?;
    let (value, after) = parse_var_ref(&arg[dollar..])?;
    // No further dynamic expressions allowed after the variable reference
    if after.contains(['$', '`', '(']) {
        return None;
    }
    Some(format!("{}{}{}", &arg[..dollar], value, after))
}

/// Parses a variable reference starting with `$`; returns (value, remainder
/// after the reference).
fn parse_var_ref(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix("${") {
        let end = rest.find('}')?;
        let value = lookup_env(&rest[..end])?;
        return Some((value, &rest[end + 1..]));
    }
    let body = s.strip_prefix('$')?;
    // `$env:NAME` (PowerShell) takes precedence over plain identifiers
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
        let dir = std::env::temp_dir().join(format!("hailux_shell_scan_{}", name));
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

        // Multiple subcommands scanned segment by segment: cd tracks the
        // resolution base; the interception point is the file-accessing segment
        let result = scan("cd /etc && cat shadow", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // In-workdir cd and subsequent commands without file access do not interrupt
        assert!(scan("cd build && ls", &wd_str).is_clean());
        assert!(scan("cd .. && ls", &wd_str).is_clean());
        assert!(
            scan("cd /etc", &wd_str).is_clean(),
            "bare cd is a no-op for the spawned shell"
        );

        // Multiple out-of-bounds targets collected in one pass (same dir deduped)
        let result = scan("cp /etc/hosts /etc/passwd .", &wd_str);
        assert_eq!(result.external_dirs.len(), 1, "same dir dedup: {result:?}");

        // The right side of a pipe is scanned too
        let result = scan("curl https://x | tee /etc/hosts", &wd_str);
        assert!(!result.external_dirs.is_empty());

        // PowerShell short aliases and spelled-out path commands are checked
        // as well. (Windows drive-letter paths are "external" only on Windows;
        // on Linux the backslash is a valid filename character and the path
        // resolves as an in-workdir relative path.)
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
        // Short aliases used inside the workdir do not false-positive
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
        // In-workdir paths produce no out-of-bounds hit
        let result = scan("python script.py", &wd_str);
        assert!(result.external_dirs.is_empty());
    }

    #[test]
    fn redirect_targets_checked_for_any_command() {
        let wd = tmp_workdir("redirect");
        let wd_str = wd.display().to_string();
        // echo is not a file command, but its redirect target must be checked
        let result = scan("echo hi > /etc/hosts", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // Operator glued to the path
        let result = scan("echo hi 2>/etc/error.log", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // fd duplication and heredoc are not paths
        assert!(scan("echo hi 2>&1", &wd_str).is_clean());
        assert!(scan("cat <<EOF", &wd_str).is_clean());
        // In-workdir redirections do not false-positive
        assert!(scan("echo hi > out.txt", &wd_str).is_clean());
    }

    #[test]
    fn known_vars_are_expanded() {
        let wd = tmp_workdir("vars");
        let wd_str = wd.display().to_string();
        // $HOME / $env: points to the home directory → necessarily out-of-bounds
        let result = scan("cat $HOME/.ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        let result = scan("cat ~/.ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        #[cfg(windows)]
        {
            let result = scan("get-content $env:USERPROFILE\\.ssh\\id_rsa", &wd_str);
            assert_eq!(result.external_dirs.len(), 1);
        }
        // Unknown variables cannot be expanded → skipped (best effort)
        assert!(scan("cat $UNKNOW_VAR/file", &wd_str).is_clean());
        // Command substitution skipped
        assert!(scan("cat $(pwd)/file", &wd_str).is_clean());
    }

    #[test]
    fn glob_parent_dir_still_gated() {
        let wd = tmp_workdir("glob");
        let wd_str = wd.display().to_string();
        // In-workdir globs are allowed
        assert!(scan("rm build/*.o", &wd_str).is_clean());
        assert!(scan("cp src/*.rs backup/", &wd_str).is_clean());
        // Out-of-bounds glob: the `..` prefix explicitly points outside, still intercepted
        assert_eq!(scan("cat ../secrets/*.pem", &wd_str).external_dirs.len(), 1);
    }

    #[test]
    fn cd_updates_resolution_base() {
        let wd = tmp_workdir("cd_base");
        let wd_str = wd.display().to_string();
        // Relative-path access after a cd out of bounds is resolved against
        // the new base → hit, with the dir being the actually accessed
        // location. The expected value is likewise normalized via
        // resolve_permission_path: the CI TEMP environment variable may
        // contain an 8.3 short name (RUNNER~1) while canonicalize yields the
        // long name; both sides must be structurally identical.
        let result = scan("cd .. && cat secret.txt", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        let expected_parent =
            resolve_permission_path(wd.parent().unwrap().to_string_lossy().as_ref(), &wd_str);
        assert_eq!(
            result.external_dirs[0], expected_parent,
            "hit dir must be the cd target, not a blanket out-of-workdir mark"
        );
        // Chained cd: the base is updated per segment
        let result = scan("cd ../.. && cat x", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // cd to the home directory then reading a sensitive file under it → hit
        let result = scan("cd ~ && cat .ssh/id_rsa", &wd_str);
        assert_eq!(result.external_dirs.len(), 1);
        // Access after cd into an in-workdir subdirectory is still safe
        assert!(scan("cd build && cat out.txt", &wd_str).is_clean());
        // `cd -` and options are not statically decidable → base unchanged, no false hit
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
