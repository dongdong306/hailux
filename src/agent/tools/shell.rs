use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;

use serde_json::{Value, json};

use super::{Tool, ToolExecuteError, external_dir_request, resolve_tool_path};
use crate::permission::PermissionRequest;
use crate::permission::bash_arity::extract_bash_pattern;
use crate::permission::bash_scan;
use crate::permission::resolve_permission_path;

/// 智能解码子进程输出：中文 Windows 上 powershell.exe 重定向管道按系统 ANSI 代码页
/// （GBK）编码输出，直接按 UTF-8 解码会产生乱码。策略：
/// 1. 通过严格 UTF-8 校验 → 原样返回（纯 ASCII / UTF-8 输出零变化）；
/// 2. 否则比较 UTF-8 lossy 与 GBK 两种解码结果中的 U+FFFD 数量，取更少者（平手取 UTF-8）。
pub(super) fn decode_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let utf8_lossy = String::from_utf8_lossy(bytes);
    let (gbk, _, _) = encoding_rs::GBK.decode(bytes);
    let utf8_errors = utf8_lossy.chars().filter(|&c| c == '\u{FFFD}').count();
    let gbk_errors = gbk.chars().filter(|&c| c == '\u{FFFD}').count();
    if gbk_errors < utf8_errors {
        gbk.into_owned()
    } else {
        utf8_lossy.into_owned()
    }
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = decode_output(stdout);
    if stdout.is_empty() {
        decode_output(stderr)
    } else {
        format!("{}\n{}", stdout, decode_output(stderr))
    }
}

/// 从 bash 工具参数中提取 `command_string`。
pub(crate) fn bash_command_from_arguments(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args["command_string"].as_str().map(str::to_string)
}

/// 从 bash 工具参数中提取可选的 `workdir`。
fn bash_workdir_from_arguments(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args["workdir"].as_str().map(str::to_string)
}

/// plan 模式下的 bash 只读拦截：返回 `Some(拒绝原因)` 时禁止执行。
/// 只读判定 fail-closed（无法明确判定为只读即拒绝）。
pub(crate) fn plan_mode_bash_denial(arguments: &str) -> Option<String> {
    let command = bash_command_from_arguments(arguments)?;
    if crate::permission::bash_readonly::is_read_only_bash_command(&command) {
        None
    } else {
        Some(format!(
            "[Bash write operation blocked in plan mode: {command}. Plan mode is read-only; exit with /plan or Shift+Tab to execute.]"
        ))
    }
}

/// Windows CREATE_NO_WINDOW：让 shell 子进程获得一块隐藏的独立控制台。
/// 若不加，pwsh 继承 hailux 的可见控制台，其启动的 GUI 子系统程序（浏览器等）
/// 在自身没有有效 std 句柄时（如经 Start-Process/ShellExecute 启动）会走
/// Chromium 式 AttachConsole(ATTACH_PARENT_PROCESS) + freopen("CONOUT$")，
/// 把日志直接写进 hailux 所在控制台的当前屏幕缓冲区，污染备用屏幕上的 TUI
/// （ratatui 是 diff 渲染，不会修复被外部写入的格子，脏区永久残留）。
/// 隐藏控制台使这类写入不可见，管道捕获不受影响；配套 stdin 置空（见
/// run_shell_command）防止子进程抢夺 TUI 键盘输入。
/// 已知副作用：PowerShell 表格按隐藏控制台默认宽度（~120 列）排版；
/// 实测在隐藏控制台内 resize 宽度行为不可靠（部分目标宽度直接失败，
/// MaxWindowSize 与实际可设值自相矛盾），勿尝试在 prologue 中调整。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows shell 程序选择：优先 PowerShell 7+（pwsh.exe），未安装时回退
/// powershell.exe。注意：pwsh 7 仅修复 PS→原生命令方向（$OutputEncoding 默认
/// UTF-8）；native→PS 解码仍按 [Console]::OutputEncoding（默认系统 GBK），
/// 因此两个版本都需要 WINDOWS_UTF8_PROLOGUE 强制双向 UTF-8。
/// 探测结果进程内缓存，只查 PATH 中文件是否存在、不额外起进程。
#[cfg(windows)]
pub(super) fn windows_shell_program() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        let has_pwsh = std::env::var_os("PATH")
            .map(|paths| {
                paths
                    .to_string_lossy()
                    .split(';')
                    .filter(|p| !p.trim().is_empty())
                    .any(|p| Path::new(p).join("pwsh.exe").is_file())
            })
            .unwrap_or(false);
        if has_pwsh {
            "pwsh.exe"
        } else {
            "powershell.exe"
        }
    })
}

/// 探测当前 Windows shell 的版本描述（如 "PowerShell 7.4.6 (pwsh.exe)"），
/// 注入 bash 工具提示词的 {SHELL_DESC} 占位符。进程内缓存、只探测一次；
/// 启动进程失败时按 shell 程序名回退到主版本静态描述。
#[cfg(windows)]
fn windows_shell_description() -> String {
    let shell = windows_shell_program();
    let (prefix, note) = if shell == "pwsh.exe" {
        ("PowerShell", "")
    } else {
        (
            "Windows PowerShell",
            "; note: && / || pipeline chains, ternary ?: and the ?? operator are not available in 5.1",
        )
    };
    let version = std::process::Command::new(shell)
        .args(windows_shell_command_args(
            "$PSVersionTable.PSVersion.ToString()",
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| decode_output(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty());
    match version {
        Some(v) => format!("{prefix} {v} ({shell}{note})"),
        None => {
            let fallback = if shell == "pwsh.exe" { "7+" } else { "5.1" };
            format!("{prefix} {fallback} ({shell}{note})")
        }
    }
}

/// Windows 命令的 UTF-8 编码 prologue。powershell.exe 在中文 Windows 上按 GBK
/// 解码原生命令的管道输出，再把自身管道输出重编码为 GBK —— 双重转码不可逆地
/// 破坏非 GBK 字符（node/cargo 的 UTF-8 中文、✓ 等），事后任何解码启发式都无法还原。
/// 从源头强制双向 UTF-8：
/// - `[Console]::OutputEncoding`：PS 读取原生命令输出与自身输出均改按 UTF-8；
/// - `$OutputEncoding`：修复反方向（PS 字符串传给原生命令，5.1 默认 ASCII 会把中文变 `?`）；
/// - UTF8Encoding($false)：BOM-less，避免重定向输出头部多出 BOM 字节；
/// - try/catch：个别宿主环境禁止修改控制台编码，失败不阻塞后续命令。
///
/// 注入发生在执行层：权限 / plan-mode / 展示仍基于原始命令，不受前缀污染。
#[cfg(windows)]
const WINDOWS_UTF8_PROLOGUE: &str = "try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding } catch { }; ";

#[cfg(windows)]
pub(super) fn wrap_windows_command(command: &str) -> String {
    format!("{WINDOWS_UTF8_PROLOGUE}{command}")
}

/// 组装 Windows shell 调用参数（含 UTF-8 prologue），执行与测试共用同一路径。
#[cfg(windows)]
pub(super) fn windows_shell_command_args(command: &str) -> Vec<String> {
    vec![
        "-NoLogo".to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        wrap_windows_command(command),
    ]
}

/// 后台预热 shell 版本描述缓存，避免首次构建工具 schema 时
/// 同步等待 shell 探测进程（约数百毫秒）。
#[cfg(windows)]
pub fn prewarm_shell_description() {
    std::thread::spawn(|| {
        let _ = BashTool.description();
    });
}

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("bash")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Vec<PermissionRequest> {
        let Some(command) = bash_command_from_arguments(arguments) else {
            return Vec::new();
        };
        if command.trim().is_empty() {
            return Vec::new();
        }
        // 指定的 workdir 在项目目录之外 → 询问
        if let Some(wd) = bash_workdir_from_arguments(arguments) {
            let wd_canonical = resolve_permission_path(&wd, work_dir);
            let work_canonical = resolve_permission_path(work_dir, work_dir);
            if !wd_canonical.starts_with(&work_canonical) {
                return vec![external_dir_request(
                    &[wd_canonical],
                    format!("Run command outside working directory: {}", wd),
                )];
            }
        }
        let (pattern, description) = extract_bash_pattern(&command);
        // 静态扫描：执行原语 → force_ask；越界路径 → external_directory 请求。
        // bash 请求在前：先评估命令规则（deny 优先），allow 后才轮到越界检查。
        let scan = bash_scan::scan(&command, work_dir);
        let mut requests = vec![PermissionRequest {
            permission: "bash".to_string(),
            patterns: vec![pattern.clone()],
            always_patterns: vec![pattern],
            description: description.clone(),
            force_ask: scan.executor_hit,
        }];
        if !scan.external_dirs.is_empty() {
            requests.push(external_dir_request(
                &scan.external_dirs,
                format!(
                    "Command accesses path outside working directory: {}",
                    description
                ),
            ));
        }
        requests
    }

    #[cfg(windows)]
    fn description(&self) -> &str {
        static DESC: OnceLock<String> = OnceLock::new();
        DESC.get_or_init(|| {
            crate::prompts::tools::BASH.replace("{SHELL_DESC}", &windows_shell_description())
        })
    }

    #[cfg(not(windows))]
    fn description(&self) -> &str {
        crate::prompts::tools::BASH
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command_string": {
                    "type": "string",
                    "description": "Command to execute"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (defaults to current directory); use this instead of cd"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds. The command will be terminated after timeout and a timeout message will be returned. Defaults to 120 seconds if not specified"
                }
            },
            "required": ["command_string"]
        })
    }

    #[cfg(windows)]
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: format!("Invalid JSON parameter: {e}"),
                })));
            }
        };
        let command = match args["command_string"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "Missing 'command_string' parameter".to_string(),
                })));
            }
        };
        let workdir = args["workdir"].as_str().map(|s| s.to_string());
        let timeout_secs = args["timeout"].as_u64().filter(|&s| s > 0).or(Some(120));
        let cmd_args = windows_shell_command_args(&command);
        // 未指定 workdir 时也会得到会话工作目录，命令不再依赖进程 cwd
        let resolved_workdir =
            Some(workdir.map_or_else(|| work_dir.to_string(), |w| resolve_tool_path(&w, work_dir)));
        Box::pin(run_shell_command(
            windows_shell_program(),
            cmd_args,
            resolved_workdir,
            timeout_secs,
        ))
    }

    #[cfg(not(windows))]
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: format!("Invalid JSON parameter: {e}"),
                })));
            }
        };
        let command = match args["command_string"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "Missing 'command_string' parameter".to_string(),
                })));
            }
        };
        let workdir = args["workdir"].as_str().map(|s| s.to_string());
        let timeout_secs = args["timeout"].as_u64().filter(|&s| s > 0).or(Some(120));
        let resolved_workdir =
            Some(workdir.map_or_else(|| work_dir.to_string(), |w| resolve_tool_path(&w, work_dir)));
        Box::pin(run_shell_command(
            "bash",
            vec!["-c".to_string(), command],
            resolved_workdir,
            timeout_secs,
        ))
    }
}

/// 进程退出后的管道排空窗口：尾部数据可能仍留在内核管道缓冲区；
/// 孙进程持有写端（如常驻 daemon）时 EOF 永不到来，必须限时。
const PIPE_DRAIN_WINDOW: Duration = Duration::from_millis(500);
/// 超时杀树后回收在途输出的窗口（等待 taskkill / kill 生效 + 排空）。
const KILL_DRAIN_WINDOW: Duration = Duration::from_millis(500);
/// 排空的硬性总上限：空闲窗口会被持续输出的孙进程无限重置，
/// 必须叠加总 deadline 兜底，否则挂起风险回归。
pub(super) const DRAIN_HARD_CAP: Duration = Duration::from_secs(3);

/// 后台读管道任务：持续读取并把字节块送入 channel，
/// 读到 EOF / 出错 / 接收端被丢弃时结束。
fn spawn_pipe_reader<R>(mut pipe: R, tx: tokio::sync::mpsc::Sender<Vec<u8>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = vec![0u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(chunk[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// 限时排空单个 channel 中残留的输出。双重保险：
/// 空闲超时（每收到一块数据重置窗口，避免传输中的大量数据被截断）
/// + 硬性总上限（持续低速输出的孙进程可无限重置空闲窗口，必须兜底）。
pub(super) async fn drain_channel_with_window(
    rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    buf: &mut Vec<u8>,
    window: Duration,
) {
    let hard_deadline = tokio::time::Instant::now() + DRAIN_HARD_CAP;
    while tokio::time::Instant::now() < hard_deadline {
        match tokio::time::timeout(window, rx.recv()).await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
}

/// Windows：taskkill 终止 pid 及其整棵进程树。直接子进程退出不代表孙进程退出——
/// 常驻 daemon / 浏览器既继承管道写端（导致 EOF 永不到来）又驻留内存。
#[cfg(windows)]
async fn kill_process_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .await;
    }
}

#[cfg(not(windows))]
async fn kill_process_tree(pid: Option<u32>) {
    // Unix 无进程树语义，由调用方对直接子进程 kill + kill_on_drop 兜底
    let _ = pid;
}

pub(super) async fn run_shell_command(
    program: &'static str,
    args: Vec<String>,
    workdir: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<String, ToolExecuteError> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.kill_on_drop(true);
    cmd.args(&args);
    if let Some(dir) = &workdir {
        cmd.current_dir(dir);
    }
    // 隐藏独立控制台 + stdin 置空，动机与副作用见 CREATE_NO_WINDOW 文档
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(std::process::Stdio::null());
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ToolExecuteError {
            message: e.to_string(),
        })?;
    let pid = child.id();
    let (stdout_tx, mut stdout_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let (stderr_tx, mut stderr_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    if let Some(pipe) = child.stdout.take() {
        spawn_pipe_reader(pipe, stdout_tx);
    }
    if let Some(pipe) = child.stderr.take() {
        spawn_pipe_reader(pipe, stderr_tx);
    }

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status: Option<std::process::ExitStatus> = None;
    let mut timed_out = false;

    let timeout_at = timeout_secs.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    let timeout_sleep = async {
        match timeout_at {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(timeout_sleep);

    // 主循环：等待进程退出（cancel-safe）与读取管道并发推进；
    // 不再要求「管道 EOF」作为完成条件，避免孙进程持写端导致永久挂起。
    loop {
        tokio::select! {
            res = child.wait() => {
                status = Some(res.map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?);
                break;
            }
            chunk = stdout_rx.recv(), if stdout_open => match chunk {
                Some(c) => stdout_buf.extend_from_slice(&c),
                None => stdout_open = false,
            },
            chunk = stderr_rx.recv(), if stderr_open => match chunk {
                Some(c) => stderr_buf.extend_from_slice(&c),
                None => stderr_open = false,
            },
            _ = &mut timeout_sleep, if timeout_at.is_some() => {
                timed_out = true;
                break;
            }
        }
    }

    if timed_out {
        // 先杀整棵树（写端全部关闭 → 管道 EOF → 在途数据可排空）
        kill_process_tree(pid).await;
        #[cfg(not(windows))]
        {
            let _ = child.start_kill();
        }
        let _ = tokio::time::timeout(KILL_DRAIN_WINDOW * 2, child.wait()).await;
        drain_channel_with_window(&mut stdout_rx, &mut stdout_buf, KILL_DRAIN_WINDOW).await;
        drain_channel_with_window(&mut stderr_rx, &mut stderr_buf, KILL_DRAIN_WINDOW).await;
    } else {
        // 进程已退出：限时排空内核缓冲区中的尾部数据（空闲超时，两侧并发）。
        // 无孙进程时 EOF 即刻到达、零额外延迟；有孙进程持写端时空闲一个窗口后放弃。
        tokio::join!(
            drain_channel_with_window(&mut stdout_rx, &mut stdout_buf, PIPE_DRAIN_WINDOW),
            drain_channel_with_window(&mut stderr_rx, &mut stderr_buf, PIPE_DRAIN_WINDOW),
        );
    }

    const MAX_OUTPUT_CHARS: usize = 10000;
    let raw = if let Some(secs) = timeout_secs.filter(|_| timed_out) {
        let marker = format!("[Command timed out ({secs}s), process tree terminated]");
        let body = combine_output(&stdout_buf, &stderr_buf);
        if body.trim().is_empty() {
            marker
        } else {
            format!("{marker}\n\n{body}")
        }
    } else {
        match &status {
            Some(st) if st.success() => decode_output(&stdout_buf),
            _ => combine_output(&stdout_buf, &stderr_buf),
        }
    };

    let char_count = raw.chars().count();
    if char_count > MAX_OUTPUT_CHARS {
        let truncated: String = raw.chars().take(MAX_OUTPUT_CHARS).collect();
        Ok(format!(
            "{}\n\n... Output truncated (showing {} of {} characters, {} more omitted)",
            truncated,
            MAX_OUTPUT_CHARS,
            char_count,
            char_count - MAX_OUTPUT_CHARS
        ))
    } else {
        Ok(raw)
    }
}
