/// Bash 命令 arity 表：前 N 个 token 作为 pattern 前缀
/// 用于从完整命令中提取人类可理解的 "命令类别"（精简版）
const BASH_ARITY: &[(&str, usize)] = &[
    // arity 1
    ("cat", 1),
    ("cd", 1),
    ("chmod", 1),
    ("chown", 1),
    ("cp", 1),
    ("echo", 1),
    ("export", 1),
    ("grep", 1),
    ("kill", 1),
    ("ln", 1),
    ("ls", 1),
    ("mkdir", 1),
    ("mv", 1),
    ("ps", 1),
    ("pwd", 1),
    ("rm", 1),
    ("rmdir", 1),
    ("sleep", 1),
    ("source", 1),
    ("tail", 1),
    ("touch", 1),
    ("which", 1),
    ("env", 1),
    ("find", 1),
    ("head", 1),
    ("wc", 1),
    ("sort", 1),
    ("uniq", 1),
    ("tee", 1),
    ("xargs", 1),
    ("curl", 1),
    ("wget", 1),
    ("ssh", 1),
    ("scp", 1),
    ("rsync", 1),
    ("sed", 1),
    ("awk", 1),
    ("date", 1),
    ("whoami", 1),
    ("hostname", 1),
    ("uname", 1),
    ("df", 1),
    ("du", 1),
    ("free", 1),
    ("top", 1),
    ("htop", 1),
    ("man", 1),
    ("less", 1),
    ("more", 1),
    ("vi", 1),
    ("vim", 1),
    ("nano", 1),
    ("history", 1),
    ("alias", 1),
    ("export", 1),
    // arity 2
    ("git", 2),
    ("npm", 2),
    ("npx", 2),
    ("pnpm", 2),
    ("yarn", 2),
    ("bun", 2),
    ("deno", 2),
    ("cargo", 2),
    ("rustup", 2),
    ("go", 2),
    ("python", 2),
    ("python3", 2),
    ("pip", 2),
    ("pipenv", 2),
    ("poetry", 2),
    ("ruby", 2),
    ("gem", 2),
    ("bundle", 2),
    ("mvn", 2),
    ("gradle", 2),
    ("make", 2),
    ("docker", 2),
    ("podman", 2),
    ("kubectl", 2),
    ("helm", 2),
    ("terraform", 2),
    ("ansible", 2),
    ("vagrant", 2),
    ("brew", 2),
    ("apt", 2),
    ("apt-get", 2),
    ("yum", 2),
    ("dnf", 2),
    ("pacman", 2),
    ("systemctl", 2),
    ("service", 2),
    ("openssl", 2),
    ("redis-cli", 2),
    ("mysql", 2),
    ("psql", 2),
    ("mongosh", 2),
    ("sqlite3", 2),
    ("gh", 2),
    ("vercel", 2),
    ("netlify", 2),
    ("flyctl", 2),
    ("swift", 2),
    ("dotnet", 2),
    ("msbuild", 2),
    ("rake", 2),
    ("composer", 2),
    ("php", 2),
    ("node", 2),
    ("tsc", 2),
    ("eslint", 2),
    ("prettier", 2),
    ("jest", 2),
    ("vitest", 2),
    ("cypress", 2),
    ("playwright", 2),
    ("webpack", 2),
    ("vite", 2),
    ("rollup", 2),
    ("esbuild", 2),
    ("turbo", 2),
    ("nx", 2),
    ("lerna", 2),
    // arity 3
    ("git config", 3),
    ("git remote", 3),
    ("git stash", 3),
    ("npm run", 3),
    ("npm exec", 3),
    ("npm init", 3),
    ("npx run", 3),
    ("pnpm run", 3),
    ("pnpm exec", 3),
    ("pnpm dlx", 3),
    ("yarn run", 3),
    ("yarn dlx", 3),
    ("bun run", 3),
    ("cargo run", 3),
    ("cargo add", 3),
    ("docker compose", 3),
    ("docker container", 3),
    ("docker image", 3),
    ("docker network", 3),
    ("docker volume", 3),
    ("docker builder", 3),
];

/// 从完整命令字符串中提取权限匹配 pattern 和展示用描述。
/// 返回 (pattern, description)：
/// - pattern：命令前 N 个 token 拼接 `*`（由 arity 表决定 N），
///   用于规则匹配与 always 持久化
/// - description：完整命令原文，用于权限弹窗展示
pub fn extract_bash_pattern(command: &str) -> (String, String) {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return ("*".to_string(), command.to_string());
    }

    // 查找最长匹配的 arity
    let mut best_arity = 1usize;
    let mut best_prefix_len = 0usize;

    for (prefix, arity) in BASH_ARITY {
        let prefix_tokens: Vec<&str> = prefix.split_whitespace().collect();
        if prefix_tokens.len() > tokens.len() {
            continue;
        }
        let matches = prefix_tokens
            .iter()
            .zip(tokens.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if matches && prefix_tokens.len() > best_prefix_len {
            best_arity = *arity;
            best_prefix_len = prefix_tokens.len();
        }
    }

    let pattern_tokens: Vec<&str> = tokens.iter().take(best_arity).cloned().collect();
    let pattern = if pattern_tokens.is_empty() {
        "*".to_string()
    } else {
        format!("{} *", pattern_tokens.join(" "))
    };

    (pattern, command.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        let (pattern, _) = extract_bash_pattern("ls -la");
        assert_eq!(pattern, "ls *");
    }

    #[test]
    fn test_git_subcommand() {
        let (pattern, _) = extract_bash_pattern("git commit -m \"hello\"");
        assert_eq!(pattern, "git commit *");
    }

    #[test]
    fn test_npm_run() {
        let (pattern, _) = extract_bash_pattern("npm run dev");
        assert_eq!(pattern, "npm run dev *");
    }

    #[test]
    fn test_docker_compose() {
        let (pattern, _) = extract_bash_pattern("docker compose up -d");
        assert_eq!(pattern, "docker compose up *");
    }

    #[test]
    fn test_unknown_command_arity1() {
        let (pattern, _) = extract_bash_pattern("myweirdtool --flag value");
        assert_eq!(pattern, "myweirdtool *");
    }

    #[test]
    fn test_empty() {
        let (pattern, desc) = extract_bash_pattern("");
        assert_eq!(pattern, "*");
        assert_eq!(desc, "");
    }
}
