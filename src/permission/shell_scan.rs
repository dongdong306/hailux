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
//!   `$env:...`) and script-assigned variables (`$n = "literal"`) are
//!   expanded, unknown ones are unknowable — prefer false negatives over
//!   nagging on common in-workdir usage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::resolve_permission_path;

/// Script-level variable assignments (`$n = "literal"`) seen so far in the
/// scanned command, keyed by lowercased name — PowerShell variables are
/// case-insensitive.
type ScriptVars = HashMap<String, String>;

/// Escape dialect of the shell that executes the command. POSIX shells
/// escape with a backslash; PowerShell escapes with a backtick only — there
/// a `\` is always literal, so `"C:\dir\"` is a complete string and not an
/// open quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellDialect {
    Posix,
    // Only constructed under the Windows dialect (plus tests); silence
    // dead_code on unix builds where clippy runs without --tests.
    #[cfg_attr(not(windows), allow(dead_code))]
    PowerShell,
}

impl ShellDialect {
    /// Matches `ShellTool`: pwsh on Windows, bash elsewhere.
    #[cfg(windows)]
    const CURRENT: Self = Self::PowerShell;
    #[cfg(not(windows))]
    const CURRENT: Self = Self::Posix;
}

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
    // PowerShell full names (every short alias below has its full name here)
    "get-content",
    "get-childitem",
    "get-item",
    "set-content",
    "set-item",
    "set-itemproperty",
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
    "gi",
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
    scan_in(command, work_dir, ShellDialect::CURRENT)
}

fn scan_in(command: &str, work_dir: &str, dialect: ShellDialect) -> ShellScan {
    let work_canonical = resolve_permission_path(work_dir, work_dir);
    let mut base_dir = work_dir.to_string();
    // Segments execute sequentially: an assignment takes effect for the
    // segments that follow it
    let mut vars = ScriptVars::new();
    let mut result = ShellScan::default();
    for seg in split_top_level_in(command, dialect) {
        if let Some((name, literal)) = assignment_target(seg) {
            vars.insert(name, literal);
        }
        scan_segment(seg, &base_dir, &work_canonical, &vars, &mut result);
        if let Some(target) = cd_target(seg)
            && let Some(expanded) = expand_candidate(&target, &vars)
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

fn scan_segment(
    seg: &str,
    base_dir: &str,
    work_canonical: &Path,
    vars: &ScriptVars,
    result: &mut ShellScan,
) {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return;
    };
    let cmd = first.trim_matches(['"', '\'']);
    match classify(cmd) {
        CmdKind::Executor => result.executor_hit = true,
        CmdKind::FileOps => scan_tokens(&tokens[1..], base_dir, work_canonical, vars, result, true),
        CmdKind::Other => scan_tokens(&tokens, base_dir, work_canonical, vars, result, false),
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
    vars: &ScriptVars,
    result: &mut ShellScan,
    include_plain_args: bool,
) {
    let mut redirect_next = false;
    for token in tokens {
        if redirect_next {
            redirect_next = false;
            check_candidate(token, base_dir, work_canonical, vars, result);
            continue;
        }
        if let Some(rest) = strip_redirect_operator(token) {
            if rest.is_empty() {
                // Standalone operator token: target is the next token
                redirect_next = true;
            } else {
                check_candidate(rest, base_dir, work_canonical, vars, result);
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
        check_candidate(arg, base_dir, work_canonical, vars, result);
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

/// Detects a script variable assignment of the form `$name = "literal"`
/// (`${name}` accepted, single or double quotes). Only whole-literal values
/// are tracked: dynamic values (`$other`, `$(...)`, concatenation) and
/// scoped names (`$env:`, `$script:`) return None.
/// Returns (lowercased name, literal value).
fn assignment_target(seg: &str) -> Option<(String, String)> {
    let (lhs, rhs) = seg.split_once('=')?;
    let name = lhs.trim().strip_prefix('$')?;
    let name = name
        .strip_prefix('{')
        .and_then(|n| n.strip_suffix('}'))
        .unwrap_or(name);
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rhs = rhs.trim();
    let quote = rhs.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rhs[1..];
    let close = inner.find(quote)?;
    // Text after the closing quote (`+ "b"`, `-Force`, ...) means this is
    // not a plain literal assignment
    if !inner[close + 1..].trim().is_empty() {
        return None;
    }
    let literal = &inner[..close];
    if literal.is_empty() || literal.contains(['$', '`', '(']) {
        return None;
    }
    Some((name.to_lowercase(), literal.to_string()))
}

/// Expands `~` and known variable references in a path candidate; dynamic
/// expressions that cannot be statically expanded (unknown `$var` / `$(...)`
/// / backticks) return None.
fn expand_candidate(arg: &str, vars: &ScriptVars) -> Option<String> {
    if arg == "~" {
        return dirs::home_dir().map(|h| h.display().to_string());
    }
    if let Some(rest) = arg.strip_prefix("~/").or_else(|| arg.strip_prefix("~\\")) {
        return dirs::home_dir().map(|h| format!("{}\\{}", h.display(), rest));
    }
    if arg.contains(['$', '(', '`']) {
        return expand_known_vars(arg, vars);
    }
    Some(arg.to_string())
}

/// Checks a single path candidate: after expanding `~` and known variables,
/// decides against the current resolution base whether it is out-of-bounds.
/// Dynamic expressions that cannot be statically expanded are skipped
/// (best effort).
fn check_candidate(
    raw: &str,
    base_dir: &str,
    work_canonical: &Path,
    vars: &ScriptVars,
    result: &mut ShellScan,
) {
    let arg = raw.trim_matches(['"', '\'']);
    if arg.is_empty() {
        return;
    }
    let Some(expanded) = expand_candidate(arg, vars) else {
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
/// The escape model follows `dialect`: a backslash escapes only in POSIX
/// shells; PowerShell escapes with a backtick alone, so a Windows path that
/// ends with `\` inside double quotes does not swallow the closing quote
/// and fuse the following commands into one segment.
fn split_top_level_in(command: &str, dialect: ShellDialect) -> Vec<&str> {
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
            '\\' if dialect == ShellDialect::Posix && !in_single => escaped = true,
            // PowerShell backtick escape (also valid inside double quotes)
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
fn expand_known_vars(arg: &str, vars: &ScriptVars) -> Option<String> {
    let dollar = arg.find('$')?;
    let (value, after) = parse_var_ref(&arg[dollar..], vars)?;
    // No further dynamic expressions allowed after the variable reference
    if after.contains(['$', '`', '(']) {
        return None;
    }
    Some(format!("{}{}{}", &arg[..dollar], value, after))
}

/// Parses a variable reference starting with `$`; returns (value, remainder
/// after the reference).
fn parse_var_ref<'a>(s: &'a str, vars: &ScriptVars) -> Option<(String, &'a str)> {
    if let Some(rest) = s.strip_prefix("${") {
        let end = rest.find('}')?;
        let value = lookup_var(&rest[..end], vars)?;
        return Some((value, &rest[end + 1..]));
    }
    let body = s.strip_prefix('$')?;
    // `$env:NAME` (PowerShell) takes precedence over plain identifiers. The
    // env scope never consults script variables — distinct PowerShell scopes.
    // `get` is None on a non-char-boundary, so multi-byte text right after
    // `$` cannot be sliced mid-character here.
    let named = match body.get(..4) {
        Some(prefix) if prefix.eq_ignore_ascii_case("env:") => {
            let (ident, after) = take_ident(&body[4..]);
            if ident.is_empty() {
                return None;
            }
            return Some((lookup_env(&ident)?, after));
        }
        _ => take_ident(body),
    };
    if named.0.is_empty() {
        return None;
    }
    Some((lookup_var(&named.0, vars)?, named.1))
}

fn take_ident(s: &str) -> (String, &str) {
    // Unicode-aware: PowerShell identifiers may contain non-ASCII letters
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (s[..end].to_string(), &s[end..])
}

/// Resolves a plain `$name` / `${name}` reference: script-assigned variables
/// shadow the environment.
fn lookup_var(name: &str, vars: &ScriptVars) -> Option<String> {
    vars.get(&name.to_lowercase())
        .cloned()
        .or_else(|| lookup_env(name))
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
            "Get-ChildItem C:\\Windows",
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
    fn non_ascii_after_dollar_does_not_panic() {
        let wd = tmp_workdir("non_ascii_dollar");
        let wd_str = wd.display().to_string();
        // '恒' straddles byte 4 of the text after `$`; the prefix check must
        // not slice mid-character and the unresolvable reference is skipped
        assert!(scan(r"cat $n\恒大/file.txt", &wd_str).is_clean());
        assert!(scan("cat ${恒}/file.txt", &wd_str).is_clean());
        assert!(scan("cat $env:恒/file.txt", &wd_str).is_clean());
        assert!(scan("cat 恒大/file.txt", &wd_str).is_clean());
    }

    #[test]
    fn script_var_assignment_is_tracked() {
        let wd = tmp_workdir("script_var");
        let wd_str = wd.display().to_string();
        // Trim the trailing separator: this test runs `scan` under the
        // platform dialect, and POSIX would treat `\"` as an escaped quote
        let outside_str = std::env::temp_dir()
            .display()
            .to_string()
            .trim_end_matches(['\\', '/'])
            .to_string();
        // `$n = "path"` then `$n\...` resolves against the assigned literal
        let result = scan(
            &format!(r#"$n = "{outside_str}" && cat $n\file.txt"#),
            &wd_str,
        );
        assert_eq!(result.external_dirs.len(), 1);
        // PowerShell variables are case-insensitive
        let result = scan(
            &format!(r#"$n = "{outside_str}" && cat $N\file.txt"#),
            &wd_str,
        );
        assert_eq!(result.external_dirs.len(), 1);
        // `${n}` reference form
        let result = scan(
            &format!(r#"$n = "{outside_str}" && cat "${{n}}\file.txt""#),
            &wd_str,
        );
        assert_eq!(result.external_dirs.len(), 1);
        // In-workdir assignment stays clean
        let result = scan(r#"$n = "sub" && cat $n\file.txt"#, &wd_str);
        assert!(result.is_clean());
        // Dynamic / concatenated values are not tracked
        assert!(scan(r#"$n = "$($other)" && cat $n\file.txt"#, &wd_str).is_clean());
        assert!(scan(r#"$n = "a" + "b" && cat $n\file.txt"#, &wd_str).is_clean());
        // Scoped names are not tracked
        assert!(scan(r#"$env:x = "a" && cat $env:x\file.txt"#, &wd_str).is_clean());
    }

    #[test]
    fn powershell_move_script_with_cjk_paths() {
        let wd = tmp_workdir("move_script");
        let wd_str = wd.display().to_string();
        let n = std::env::temp_dir();
        let n_str = n
            .display()
            .to_string()
            .trim_end_matches(['\\', '/'])
            .to_string();
        // Real agent-generated script shape (sanitized paths): a variable
        // assignment plus multi-line Move-Item references with CJK segments.
        // This used to panic in parse_var_ref ('恒' straddling the byte-4
        // slice of the text after `$`).
        let command = format!(
            "$n = \"{n_str}\"\n\
             Move-Item -LiteralPath \"$n\\恒大地产集团南宁有限公司破产清算一案.pdf\" -Destination \"$n\\公告\" -Force\n\
             Move-Item -LiteralPath \"$n\\7.线上债权申报指南（如采用法院系统线上申报）.docx\" -Destination \"$n\\申报指引\" -Force\n\
             Get-ChildItem -LiteralPath $n | Select-Object -ExpandProperty Name"
        );
        let result = scan(&command, &wd_str);
        assert!(!result.executor_hit);
        assert!(!result.external_dirs.is_empty());
    }

    #[test]
    fn powershell_trailing_backslash_assignment_still_gates() {
        let wd = tmp_workdir("trailing_bs");
        let wd_str = wd.display().to_string();
        // `$n` ends with a separator: under the PowerShell dialect the
        // closing quote is not swallowed, the assignment is tracked and the
        // later `$n\...` references resolve to the out-of-workdir literal
        let n = std::env::temp_dir()
            .display()
            .to_string()
            .trim_end_matches(['\\', '/'])
            .to_string()
            + "/";
        let command = format!(
            "$n = \"{n}\"\nMove-Item -LiteralPath \"$n\\公告\\x.pdf\" -Destination \"$n\\y\" -Force"
        );
        let result = scan_in(&command, &wd_str, ShellDialect::PowerShell);
        assert!(!result.executor_hit);
        assert!(!result.external_dirs.is_empty());
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

    fn split_posix(c: &str) -> Vec<&str> {
        split_top_level_in(c, ShellDialect::Posix)
    }

    fn split_pwsh(c: &str) -> Vec<&str> {
        split_top_level_in(c, ShellDialect::PowerShell)
    }

    #[test]
    fn quoted_separators_are_not_split() {
        let sp = split_posix;
        assert_eq!(sp("echo 'a && b' && cat /etc/hosts").len(), 2);
        assert_eq!(sp("echo \"x; y\" ; ls").len(), 2);
        assert_eq!(sp("echo 'it''s fine'").len(), 1);
        assert_eq!(sp("printf '%s\\n' 'a|b'").len(), 1);
        assert!(sp("").is_empty());
        assert!(sp("   ").is_empty());
        // POSIX: `\"` inside double quotes is a literal quote, the string
        // stays open and the `;` does not split
        assert_eq!(sp(r#"echo "a\"; cat x"#).len(), 1);
        // POSIX: `\;` outside quotes is a literal semicolon
        assert_eq!(sp(r"echo a\;b; cat x").len(), 2);
    }

    #[test]
    fn powershell_backslash_is_not_an_escape() {
        let sp = split_pwsh;
        // `"a\"` is a complete string in PowerShell: the trailing `\` is
        // literal, the quote closes and `;` splits
        assert_eq!(sp(r#"echo "a\"; cat x"#).len(), 2);
        // A Windows path ending with `\` no longer fuses the following
        // commands into one segment
        assert_eq!(sp("$n = \"C:\\dir\\\"; cat $n\\x").len(), 2);
        // The backtick remains the escape, including inside double quotes
        assert_eq!(sp(r#""a`;b"; cat x"#).len(), 2);
        // Quoted separators still do not split
        assert_eq!(sp("echo 'a && b' && cat x").len(), 2);
        assert_eq!(sp(r#"echo "x; y" ; ls"#).len(), 2);
    }

    #[test]
    fn classify_covers_both_shells() {
        assert_eq!(classify("cat"), CmdKind::FileOps);
        assert_eq!(classify("Get-Content"), CmdKind::FileOps);
        assert_eq!(classify("Get-ChildItem"), CmdKind::FileOps);
        assert_eq!(classify("Get-Item"), CmdKind::FileOps);
        assert_eq!(classify("Set-ItemProperty"), CmdKind::FileOps);
        assert_eq!(classify("git"), CmdKind::Other);
        assert_eq!(classify("sudo"), CmdKind::Executor);
        assert_eq!(classify("PYTHON3"), CmdKind::Executor);
    }
}
