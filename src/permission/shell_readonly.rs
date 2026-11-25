//! Read-only command determination for plan mode.
//!
//! Semantics are **fail-closed**: a command that cannot be proven read-only
//! is treated as a write. Three layers of checks:
//! 1. Output redirection (`>` / `>>`, except fd merges like `2>&1` and
//!    targets like `/dev/null`) → write
//! 2. Split into segments by `;` `&&` `||` `|` `&` and newlines; every
//!    segment must be read-only
//! 3. Each segment is matched against a read-only whitelist by command name,
//!    refined by subcommand / flags (git, cargo, sed, find, sort, ...)

/// Read-only command modifier prefixes (consumed before evaluating the real command name).
const MODIFIERS: &[&str] = &["sudo", "time", "command", "nohup"];

/// General read-only command whitelist (case-insensitive).
const READ_ONLY_COMMANDS: &[&str] = &[
    // Directory / system info
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
    // Processes / help
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
    // File reading
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
    // Text search / processing (read-only without output redirection)
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
    // PowerShell read-only cmdlets / aliases
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

/// Strips quoted content (replaced with same-length spaces) so that
/// `--grep="a>b"`, `echo "a|b"` etc. are not mistaken for redirection / separators.
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

/// Removes fd-merge tokens (`2>&1`, `1>&2`, `>&1`, ...) entirely so that the
/// `&` inside them is not mistaken for a command separator nor the `>` for an
/// output redirection. Later logic re-tokenizes anyway, so positions do not
/// matter.
fn mask_fd_merges(stripped: &str) -> String {
    stripped
        .split_whitespace()
        .filter(|t| !t.contains(">&"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Checks whether an output redirection that writes to a file exists.
/// - fd merges like `2>&1` / `1>&2` / `>&1` → allowed (already removed by `mask_fd_merges`)
/// - Target is `/dev/null`, `NUL`, `$null` → allowed (discarding output is not a write)
/// - Any other `>` / `>>` / `2>` etc. → write
fn has_output_redirect(stripped: &str) -> bool {
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        let Some(gt) = token.find('>') else { continue };
        // Target: the part after `>` (e.g. `>file`, `>>log`), or the next token (e.g. `> file`)
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

/// Splits into segments by command-chain separators. Quoted content has
/// already been flattened by `strip_quoted`, so it cannot be mis-split.
fn split_segments(stripped: &str) -> Vec<&str> {
    stripped
        .split([';', '&', '|', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Environment variable assignment prefix of the form `FOO=bar`.
fn is_assignment(token: &str) -> bool {
    !token.starts_with('-') && token.contains('=')
}

fn git_is_read_only(rest: &[&str]) -> bool {
    let Some(sub) = rest.first() else {
        return true; // bare `git` → help / version → read-only
    };
    let sub = sub.to_ascii_lowercase();
    let args = &rest[1..];
    match sub.as_str() {
        "status" | "log" | "diff" | "show" | "ls-files" | "rev-parse" | "describe" | "blame"
        | "grep" | "shortlog" | "help" | "version" | "ls-remote" | "show-ref" | "for-each-ref"
        | "name-rev" | "merge-base" | "cherry" | "whatchanged" | "diff-tree" | "verify-pack" => {
            true
        }
        // Branch: bare / read-only listing flags; branch-name arguments
        // (create/delete/rename/force) → write
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
        // Tag: bare / `-l`/`--list` (optional pattern) → read-only;
        // anything else (-a/-s/-d/-f/-m and bare-name creation) → write
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
        // stash: only list / show
        "stash" => {
            args.is_empty()
                || args
                    .first()
                    .is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "list" | "show"))
        }
        // remote: only -v / show / get-url
        "remote" => {
            args.is_empty()
                || args.first().is_some_and(|t| {
                    let t = t.to_ascii_lowercase();
                    matches!(t.as_str(), "-v" | "show" | "get-url")
                })
        }
        // config: query forms only (--get/--list family, or a single key with no value)
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
        return true; // bare `cargo` → help → read-only
    };
    let sub = sub.to_ascii_lowercase();
    if matches!(
        sub.as_str(),
        "metadata" | "tree" | "search" | "info" | "locate-project" | "help" | "version" | "explain"
    ) {
        return true;
    }
    // Pure-flag forms like `cargo --version` / `cargo --list`
    sub.starts_with('-') && rest.iter().all(|t| t.starts_with('-'))
}

fn segment_is_read_only(segment: &str) -> bool {
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    if tokens.is_empty() {
        return true;
    }
    // Skip environment variable assignment prefixes (`FOO=bar cmd`)
    let mut i = 0;
    while i < tokens.len() && is_assignment(tokens[i]) {
        i += 1;
    }
    // Skip read-only modifier prefixes (`sudo git status`)
    while i < tokens.len() && MODIFIERS.contains(&tokens[i].to_ascii_lowercase().as_str()) {
        i += 1;
    }
    let Some(cmd) = tokens.get(i) else {
        return false; // assignments / modifiers only, no actual command → fail-closed
    };
    let cmd = cmd.to_ascii_lowercase();
    let rest = &tokens[i + 1..];
    match cmd.as_str() {
        "git" => git_is_read_only(rest),
        "cargo" => cargo_is_read_only(rest),
        // sed with -i / --in-place modifies the file directly → write
        "sed" => !rest.iter().any(|t| {
            let t = t.to_ascii_lowercase();
            t == "-i" || t == "--in-place" || (t.starts_with("-i") && t.len() > 2)
        }),
        // find's -delete / -exec / -execdir / -ok modify content → write
        "find" => !rest.iter().any(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok"
            )
        }),
        // sort / uniq with -o write output to a file → write
        "sort" | "uniq" => !rest.contains(&"-o"),
        cmd if READ_ONLY_COMMANDS.contains(&cmd) => true,
        _ => false,
    }
}

/// Returns whether a shell command is read-only for plan mode.
/// Fail-closed: a command that cannot be proven read-only returns `false`.
pub fn is_read_only_shell_command(command: &str) -> bool {
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
            assert!(is_read_only_shell_command(cmd), "expected read-only: {cmd}");
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
                !is_read_only_shell_command(cmd),
                "expected write-denied: {cmd}"
            );
        }
    }

    #[test]
    fn edge_cases() {
        assert!(is_read_only_shell_command(""));
        assert!(is_read_only_shell_command("   "));
        assert!(is_read_only_shell_command("2>&1 git log"));
        assert!(!is_read_only_shell_command("echo a > out && echo b"));
        assert!(!is_read_only_shell_command("git log > /tmp/out.txt"));
    }
}
