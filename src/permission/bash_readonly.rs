//! plan 模式下的 bash 只读命令判定。
//!
//! 语义为 **fail-closed**：无法明确判定为只读的命令一律视为写操作。
//! 判定分三层：
//! 1. 输出重定向（`>` / `>>`，fd 合并 `2>&1` 与 `/dev/null` 等目标除外）→ 写
//! 2. 按 `;` `&&` `||` `|` `&` 换行拆段，每段都必须只读
//! 3. 每段按命令名查只读白名单，并细化子命令 / 标志位（git、cargo、sed、find、sort 等）

/// 只读命令修饰符前缀（消耗后继续判定真正的命令名）。
const MODIFIERS: &[&str] = &["sudo", "time", "command", "nohup"];

/// 通用只读命令白名单（大小写不敏感）。
const READ_ONLY_COMMANDS: &[&str] = &[
    // 目录 / 系统信息
    "ls",
    "dir",
    "pwd",
    "whoami",
    "id",
    "hostname",
    "uname",
    "date",
    "df",
    "du",
    "free",
    "uptime",
    "w",
    "users",
    "groups",
    "getent",
    "printenv",
    "tput",
    "clear",
    "cls",
    // 进程 / 帮助
    "ps",
    "top",
    "htop",
    "pstree",
    "pgrep",
    "jobs",
    "which",
    "type",
    "where",
    "man",
    "less",
    "more",
    "history",
    "help",
    "whatis",
    "apropos",
    "whereis",
    // 文件读取
    "cat",
    "head",
    "tail",
    "tac",
    "rev",
    "wc",
    "fold",
    "nl",
    "pr",
    "file",
    "stat",
    "tree",
    "lsattr",
    "getfacl",
    "readlink",
    "realpath",
    "basename",
    "dirname",
    "strings",
    "xxd",
    "od",
    "hexdump",
    "base64",
    "iconv",
    "expand",
    "unexpand",
    // 文本搜索 / 处理（无输出重定向时为只读）
    "grep",
    "rg",
    "find",
    "diff",
    "cmp",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "cut",
    "tr",
    "jq",
    "sort",
    "uniq",
    "sed",
    "awk",
    "shuf",
    "seq",
    "paste",
    "join",
    "comm",
    "column",
    "tsort",
    "echo",
    "printf",
    "test",
    "true",
    "false",
    // PowerShell 只读 cmdlet / 别名
    "get-childitem",
    "get-content",
    "get-item",
    "get-location",
    "get-process",
    "get-command",
    "get-help",
    "get-date",
    "get-filehash",
    "get-itemproperty",
    "get-alias",
    "get-history",
    "get-service",
    "get-member",
    "get-psdrive",
    "get-volume",
    "get-computerinfo",
    "select-string",
    "test-path",
    "measure-object",
    "write-output",
    "write-host",
    "format-table",
    "out-string",
    "select-object",
    "sort-object",
    "where-object",
    "group-object",
    "findstr",
];

/// 剔除引号包裹的内容（替换为等长空格），
/// 避免 `--grep="a>b"`、`echo "a|b"` 等被误判为重定向 / 分隔符。
fn strip_quoted(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut quote: Option<char> = None;
    for c in input.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                out.push(' ');
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// 将 fd 合并 token（`2>&1`、`1>&2`、`>&1` 等）整体剔除，
/// 避免其中的 `&` 被误判为命令分隔符、`>` 被误判为输出重定向。
/// 后续逻辑均重新分词，位置信息无关紧要。
fn mask_fd_merges(stripped: &str) -> String {
    stripped
        .split_whitespace()
        .filter(|t| !t.contains(">&"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 判断是否存在「写入文件」的输出重定向。
/// - `2>&1` / `1>&2` / `>&1` 等 fd 合并 → 放行（已由 `mask_fd_merges` 剔除）
/// - 目标为 `/dev/null`、`NUL`、`$null` → 放行（丢弃输出不算写）
/// - 其余 `>` / `>>` / `2>` 等 → 写
fn has_output_redirect(stripped: &str) -> bool {
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        let Some(gt) = token.find('>') else { continue };
        // 目标：`>` 之后的部分（如 `>file`、`>>log`），或下一个 token（如 `> file`）
        let mut target = token[gt + 1..].trim_start_matches('>').to_string();
        if target.is_empty()
            && let Some(next) = tokens.get(i + 1)
        {
            target = next.trim_matches(['\'', '"']).to_string();
        }
        let target = target.trim_matches(['\'', '"']).to_lowercase();
        if !matches!(target.as_str(), "/dev/null" | "nul" | "$null" | "") {
            return true;
        }
    }
    false
}

/// 按命令链分隔符拆段。引号内容已由 `strip_quoted` 打平，不会误拆。
fn split_segments(stripped: &str) -> Vec<&str> {
    stripped
        .split([';', '&', '|', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// 形如 `FOO=bar` 的环境变量赋值前缀。
fn is_assignment(token: &str) -> bool {
    !token.starts_with('-') && token.contains('=')
}

fn git_is_read_only(rest: &[&str]) -> bool {
    let Some(sub) = rest.first() else {
        return true; // 裸 `git` → 帮助 / 版本 → 只读
    };
    let sub = sub.to_ascii_lowercase();
    let args = &rest[1..];
    match sub.as_str() {
        "status" | "log" | "diff" | "show" | "ls-files" | "rev-parse" | "describe" | "blame"
        | "grep" | "shortlog" | "help" | "version" | "ls-remote" | "show-ref" | "for-each-ref"
        | "name-rev" | "merge-base" | "cherry" | "whatchanged" | "diff-tree" | "verify-pack" => {
            true
        }
        // 分支：裸 / 只读列表标志；带分支名参数（创建/删除/改名/强制）→ 写
        "branch" => {
            args.is_empty()
                || args.iter().all(|t| {
                    let t = t.to_ascii_lowercase();
                    matches!(
                        t.as_str(),
                        "-v" | "-vv" | "-a" | "-r" | "-l" | "--all" | "--list" | "--color"
                    )
                })
        }
        // 标签：裸 / `-l`/`--list`（可带 pattern）→ 只读；其余（-a/-s/-d/-f/-m 及裸名创建）→ 写
        "tag" => {
            args.is_empty()
                || (args.iter().all(|t| {
                    let t = t.to_ascii_lowercase();
                    matches!(
                        t.as_str(),
                        "-l" | "--list" | "--sort" | "--format" | "--color"
                    )
                }) && (args.iter().any(|t| {
                    let t = t.to_ascii_lowercase();
                    matches!(t.as_str(), "-l" | "--list")
                }) || args.iter().all(|t| t.starts_with('-'))))
        }
        // stash：仅 list / show
        "stash" => {
            args.is_empty()
                || args
                    .first()
                    .is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "list" | "show"))
        }
        // remote：仅 -v / show / get-url
        "remote" => {
            args.is_empty()
                || args.first().is_some_and(|t| {
                    let t = t.to_ascii_lowercase();
                    matches!(t.as_str(), "-v" | "show" | "get-url")
                })
        }
        // config：仅查询形式（--get/--list 系列，或单个 key 无值）
        "config" => {
            if args.iter().any(|t| {
                let t = t.to_ascii_lowercase();
                matches!(
                    t.as_str(),
                    "--add" | "--unset" | "--unset-all" | "--remove-section" | "--rename-section"
                )
            }) {
                return false;
            }
            args.iter().any(|t| {
                let t = t.to_ascii_lowercase();
                matches!(
                    t.as_str(),
                    "--get"
                        | "--get-all"
                        | "--get-regexp"
                        | "--list"
                        | "-l"
                        | "--global"
                        | "--local"
                        | "--system"
                        | "--file"
                )
            }) || args.iter().filter(|t| !t.starts_with('-')).count() <= 1
        }
        _ => false,
    }
}

fn cargo_is_read_only(rest: &[&str]) -> bool {
    let Some(sub) = rest.first() else {
        return true; // 裸 `cargo` → 帮助 → 只读
    };
    let sub = sub.to_ascii_lowercase();
    if matches!(
        sub.as_str(),
        "metadata" | "tree" | "search" | "info" | "locate-project" | "help" | "version" | "explain"
    ) {
        return true;
    }
    // `cargo --version` / `cargo --list` 等纯标志形式
    sub.starts_with('-') && rest.iter().all(|t| t.starts_with('-'))
}

fn segment_is_read_only(segment: &str) -> bool {
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    if tokens.is_empty() {
        return true;
    }
    // 跳过环境变量赋值前缀（`FOO=bar cmd`）
    let mut i = 0;
    while i < tokens.len() && is_assignment(tokens[i]) {
        i += 1;
    }
    // 跳过只读修饰符前缀（`sudo git status`）
    while i < tokens.len() && MODIFIERS.contains(&tokens[i].to_ascii_lowercase().as_str()) {
        i += 1;
    }
    let Some(cmd) = tokens.get(i) else {
        return false; // 只有赋值 / 修饰符，没有实际命令 → fail-closed
    };
    let cmd = cmd.to_ascii_lowercase();
    let rest = &tokens[i + 1..];
    match cmd.as_str() {
        "git" => git_is_read_only(rest),
        "cargo" => cargo_is_read_only(rest),
        // sed 带 -i / --in-place 直接改文件 → 写
        "sed" => !rest.iter().any(|t| {
            let t = t.to_ascii_lowercase();
            t == "-i" || t == "--in-place" || (t.starts_with("-i") && t.len() > 2)
        }),
        // find 的 -delete / -exec / -execdir / -ok 会修改内容 → 写
        "find" => !rest.iter().any(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok"
            )
        }),
        // sort / uniq 的 -o 输出到文件 → 写
        "sort" | "uniq" => !rest.contains(&"-o"),
        cmd if READ_ONLY_COMMANDS.contains(&cmd) => true,
        _ => false,
    }
}

/// plan 模式下判断 bash 命令是否为只读操作。
/// fail-closed：无法明确判定为只读的命令返回 `false`。
pub fn is_read_only_bash_command(command: &str) -> bool {
    let stripped = strip_quoted(command);
    let masked = mask_fd_merges(&stripped);
    if has_output_redirect(&masked) {
        return false;
    }
    split_segments(&masked)
        .iter()
        .all(|seg| segment_is_read_only(seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_commands_allowed() {
        for cmd in [
            "git status",
            "git status --short",
            "git log --oneline -5",
            "git diff HEAD",
            "git diff --stat",
            "git show HEAD~1",
            "git branch",
            "git branch -v",
            "git tag",
            "git tag -l \"v1*\"",
            "git remote -v",
            "git remote show origin",
            "git config user.name",
            "git config --global --list",
            "git stash list",
            "git ls-files",
            "git rev-parse HEAD",
            "git grep TODO",
            "ls -la",
            "pwd",
            "whoami",
            "cat Cargo.toml",
            "grep -r foo src",
            "head -20 README.md",
            "wc -l src/main.rs",
            "cargo metadata --no-deps",
            "cargo tree",
            "cargo --version",
            "cargo search serde",
            "find . -name '*.rs'",
            "git log --grep=\"a>b\"",
            "git log 2>&1 | head -5",
            "git status > /dev/null",
            "cat x >> /dev/null",
            "git status && git log",
            "cat file | grep foo",
            "FOO=bar git status",
            "sudo git status",
            "echo hello",
            "Get-Content file.txt",
            "Get-ChildItem -Recurse",
            "Select-String foo file.txt",
            "git status; git diff",
        ] {
            assert!(is_read_only_bash_command(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn write_commands_denied() {
        for cmd in [
            "rm -rf target",
            "git commit -m x",
            "git push",
            "git pull",
            "git checkout main",
            "git reset --hard HEAD",
            "git branch -d old",
            "git tag -d v1",
            "git tag v1.0",
            "git tag -a v1 -m msg",
            "git stash pop",
            "git stash push",
            "git remote add origin x",
            "git config user.name me",
            "git config --add a b",
            "git clean -fd",
            "git submodule update",
            "mv a b",
            "mkdir -p foo",
            "touch file",
            "chmod +x script.sh",
            "echo hi > file.txt",
            "echo hi >> log",
            "cat a > b",
            "cat x 2> err.txt",
            "sed -i 's/a/b/' f",
            "sed --in-place 's/a/b/' f",
            "npm install",
            "npm run build",
            "cargo build",
            "cargo test",
            "cargo check",
            "cargo clippy",
            "cargo fmt",
            "cargo run",
            "cargo add serde",
            "cargo clean",
            "cargo install ripgrep",
            "find . -delete",
            "find . -exec rm {} \\;",
            "sort -o out.txt f",
            "tee out.txt",
            "xargs rm",
            "Set-Content file.txt x",
            "Remove-Item file.txt",
            "New-Item -Path x",
            "Copy-Item a b",
            "Out-File -FilePath x",
            "python3 -c 'print(1)'",
            "docker build -t x .",
            "git status && rm -rf x",
            "grep foo f | tee out",
            "sudo rm -rf /",
            "env FOO=bar rm x",
        ] {
            assert!(
                !is_read_only_bash_command(cmd),
                "expected write-denied: {cmd}"
            );
        }
    }

    #[test]
    fn edge_cases() {
        assert!(is_read_only_bash_command(""));
        assert!(is_read_only_bash_command("   "));
        assert!(is_read_only_bash_command("2>&1 git log"));
        assert!(!is_read_only_bash_command("echo a > out && echo b"));
        assert!(!is_read_only_bash_command("git log > /tmp/out.txt"));
    }
}
