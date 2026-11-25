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
use crate::permission::resolve_permission_path;
use crate::permission::shell_arity::extract_shell_pattern;
use crate::permission::shell_scan;

/// Smartly decodes subprocess output: on Chinese Windows, powershell.exe encodes
/// redirected pipe output in the system ANSI code page (GBK); decoding it directly
/// as UTF-8 produces mojibake. Strategy:
/// 1. Passes strict UTF-8 validation → return as-is (pure ASCII / UTF-8 output
///    is unchanged);
/// 2. Otherwise compare the U+FFFD counts of the UTF-8 lossy and GBK decodings
///    and take the one with fewer errors (ties go to UTF-8).
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

/// Extracts `command_string` from shell tool arguments.
pub(crate) fn shell_command_from_arguments(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args["command_string"].as_str().map(str::to_string)
}

/// Extracts the optional `workdir` from shell tool arguments.
fn shell_workdir_from_arguments(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args["workdir"].as_str().map(str::to_string)
}

/// Plan-mode read-only gate for shell: `Some(denial reason)` forbids execution.
/// The read-only check is fail-closed (not provably read-only means denied).
pub(crate) fn plan_mode_shell_denial(arguments: &str) -> Option<String> {
    let command = shell_command_from_arguments(arguments)?;
    if crate::permission::shell_readonly::is_read_only_shell_command(&command) {
        None
    } else {
        Some(format!(
            "[Shell write operation blocked in plan mode: {command}. Plan mode is read-only; exit with /plan or Shift+Tab to execute.]"
        ))
    }
}

/// Windows CREATE_NO_WINDOW: gives the shell subprocess a hidden console of its
/// own. Without it, pwsh inherits hailux's visible console, and GUI-subsystem
/// programs it launches (browsers etc.) that lack valid std handles of their own
/// (e.g. when started via Start-Process/ShellExecute) take the Chromium-style
/// AttachConsole(ATTACH_PARENT_PROCESS) + freopen("CONOUT$") route, writing logs
/// straight into the current screen buffer of hailux's console and polluting the
/// TUI on the alternate screen (ratatui renders by diff and will not repair cells
/// written externally, so dirty areas stay permanently).
/// The hidden console makes such writes invisible; pipe capture is unaffected.
/// The paired stdin nulling (see run_shell_command) prevents the subprocess from
/// stealing TUI keyboard input.
/// Known side effect: PowerShell tables are laid out at the hidden console's
/// default width (~120 columns); in practice resizing the width inside a hidden
/// console is unreliable (some target widths fail outright, and MaxWindowSize
/// contradicts the actually settable values), so do not try to adjust it in the
/// prologue.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows shell program selection: prefer PowerShell 7+ (pwsh.exe), fall back
/// to powershell.exe when it is not installed. Note: pwsh 7 only fixes the
/// PS→native direction ($OutputEncoding defaults to UTF-8); native→PS decoding
/// still follows [Console]::OutputEncoding (system GBK by default), so both
/// versions need WINDOWS_UTF8_PROLOGUE to force UTF-8 in both directions.
/// The probe result is cached in-process; it only checks for file existence on
/// PATH, without spawning an extra process.
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

/// Probes the current Windows shell's version description (e.g.
/// "PowerShell 7.4.6 (pwsh.exe)") for the {SHELL_DESC} placeholder in the
/// shell tool prompt. Cached in-process and probed only once; falls back to
/// a static major-version description by program name when probing fails.
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

/// UTF-8 encoding prologue for Windows commands. On Chinese Windows,
/// powershell.exe decodes native commands' pipe output as GBK, then re-encodes
/// its own pipe output as GBK — the double transcoding irreversibly corrupts
/// non-GBK characters (node/cargo's UTF-8 Chinese text, ✓, etc.); no decoding
/// heuristic afterwards can restore them. Forces UTF-8 in both directions at
/// the source:
/// - `[Console]::OutputEncoding`: switches both PS reading of native command
///   output and PS's own output to UTF-8;
/// - `$OutputEncoding`: fixes the reverse direction (PS strings passed to native
///   commands; 5.1's ASCII default turns Chinese into `?`);
/// - UTF8Encoding($false): BOM-less, avoiding extra BOM bytes at the head of
///   redirected output;
/// - try/catch: some host environments forbid changing the console encoding;
///   failure must not block the subsequent commands.
///
/// Injection happens at the execution layer: permissions / plan-mode / display
/// remain based on the original command and are not polluted by the prefix.
#[cfg(windows)]
const WINDOWS_UTF8_PROLOGUE: &str = "try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding } catch { }; ";

#[cfg(windows)]
pub(super) fn wrap_windows_command(command: &str) -> String {
    format!("{WINDOWS_UTF8_PROLOGUE}{command}")
}

/// Assembles the Windows shell invocation args (including the UTF-8 prologue);
/// execution and tests share the same path.
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

/// Prewarms the shell version description cache in the background, so the first
/// tool-schema build does not synchronously wait on the shell probe process
/// (several hundred milliseconds).
#[cfg(windows)]
pub fn prewarm_shell_description() {
    std::thread::spawn(|| {
        let _ = ShellTool.description();
    });
}

pub struct ShellTool;

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("shell")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Vec<PermissionRequest> {
        let Some(command) = shell_command_from_arguments(arguments) else {
            return Vec::new();
        };
        if command.trim().is_empty() {
            return Vec::new();
        }
        // The given workdir is outside the project directory → ask
        if let Some(wd) = shell_workdir_from_arguments(arguments) {
            let wd_canonical = resolve_permission_path(&wd, work_dir);
            let work_canonical = resolve_permission_path(work_dir, work_dir);
            if !wd_canonical.starts_with(&work_canonical) {
                return vec![external_dir_request(
                    &[wd_canonical],
                    format!("Run command outside working directory: {}", wd),
                )];
            }
        }
        let (pattern, description) = extract_shell_pattern(&command);
        // Static scan: execution primitive → force_ask; out-of-bounds path →
        // external_directory request.
        // Shell request first: command rules are evaluated before the
        // out-of-workdir check (deny wins; out-of-workdir only after allow).
        let scan = shell_scan::scan(&command, work_dir);
        let mut requests = vec![PermissionRequest {
            permission: "shell".to_string(),
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
            crate::prompts::tools::SHELL.replace("{SHELL_DESC}", &windows_shell_description())
        })
    }

    #[cfg(not(windows))]
    fn description(&self) -> &str {
        crate::prompts::tools::SHELL
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
        // Without a workdir argument the session working directory is still used,
        // so commands no longer depend on the process cwd
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

/// Pipe drain window after process exit: tail data may still sit in the kernel
/// pipe buffer; when a grandchild holds the write end (e.g. a resident daemon),
/// EOF never arrives, so the drain must be time-limited.
const PIPE_DRAIN_WINDOW: Duration = Duration::from_millis(500);
/// Window for reclaiming in-flight output after a timeout kill of the tree
/// (waiting for taskkill / kill to take effect + draining).
const KILL_DRAIN_WINDOW: Duration = Duration::from_millis(500);
/// Hard overall cap on draining: the idle window can be reset indefinitely by a
/// grandchild that keeps producing output, so an overall deadline is needed as
/// a backstop, otherwise the hang risk regresses.
pub(super) const DRAIN_HARD_CAP: Duration = Duration::from_secs(3);

/// Background pipe-reading task: keeps reading and sends byte chunks into the
/// channel; ends on EOF / error / dropped receiver.
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

/// Time-limited drain of output left in a single channel. Double protection:
/// idle timeout (the window resets on every received chunk so in-transit bulk
/// data is not truncated) plus hard overall cap (a grandchild producing slow
/// but steady output can reset the idle window indefinitely; a backstop is
/// mandatory).
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

/// Windows: taskkill terminates the pid and its whole process tree. A direct
/// child exiting does not mean grandchildren exited — resident daemons /
/// browsers both inherit the pipe write end (so EOF never arrives) and stay in
/// memory.
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
    // Unix has no process-tree semantics; the caller backstops with a
    // direct-child kill + kill_on_drop
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
    // Hidden dedicated console + nulled stdin; see the CREATE_NO_WINDOW docs
    // for motivation and side effects
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

    // Main loop: waiting for process exit (cancel-safe) and reading the pipes
    // progress concurrently; pipe EOF is no longer required as the completion
    // condition, avoiding a permanent hang when a grandchild holds the write end.
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
        // Kill the whole tree first (write ends all close → pipe EOF → in-flight data drainable)
        kill_process_tree(pid).await;
        #[cfg(not(windows))]
        {
            let _ = child.start_kill();
        }
        let _ = tokio::time::timeout(KILL_DRAIN_WINDOW * 2, child.wait()).await;
        drain_channel_with_window(&mut stdout_rx, &mut stdout_buf, KILL_DRAIN_WINDOW).await;
        drain_channel_with_window(&mut stderr_rx, &mut stderr_buf, KILL_DRAIN_WINDOW).await;
    } else {
        // Process already exited: drain tail data from the kernel buffers with a
        // time limit (idle timeout, both sides concurrently).
        // Without grandchildren EOF arrives immediately at zero extra cost; with
        // a grandchild holding the write end, give up after one idle window.
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
