use super::bash::{DRAIN_HARD_CAP, drain_channel_with_window};
use super::bash::{decode_output, run_shell_command};
#[cfg(windows)]
use super::bash::{windows_shell_command_args, windows_shell_program, wrap_windows_command};
use super::diff::serialize_diff_data;
use super::*;
use crate::permission::normalize_lexical;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;

fn tmp_workdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hailux_perm_test_{}", name));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
fn diff_data_small_change_not_truncated() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    let new = "a\nb\nX\nd\ne\nf\ng\nh\ni\nj\n";
    let v: serde_json::Value =
        serde_json::from_str(&serialize_diff_data(Some(old), new, "src/main.rs")).unwrap();
    assert_eq!(v["format"], "diff");
    assert_eq!(v["path"], "src/main.rs");
    assert_eq!(v["additions"], 1);
    assert_eq!(v["deletions"], 1);
    assert_eq!(v["old_lines"], 10);
    assert_eq!(v["new_lines"], 10);
    assert_eq!(v["is_new_file"], false);
    let hunks = v["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0]["old_start"], 1);
    assert_eq!(hunks[0]["new_start"], 1);
    let changes = hunks[0]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 7); // 2 前置上下文 + 1 删 + 1 增 + 3 后置上下文
    let signs: Vec<&str> = changes.iter().map(|c| c[0].as_str().unwrap()).collect();
    assert_eq!(signs, vec![" ", " ", "-", "+", " ", " ", " "]);
}

#[test]
fn diff_data_new_file() {
    let v: serde_json::Value =
        serde_json::from_str(&serialize_diff_data(None, "x\ny\n", "a.txt")).unwrap();
    assert_eq!(v["is_new_file"], true);
    assert_eq!(v["additions"], 2);
    assert_eq!(v["deletions"], 0);
    assert_eq!(v["old_lines"], 0);
    let hunks = v["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0]["old_start"], serde_json::Value::Null);
    assert_eq!(hunks[0]["new_start"], 1);
    let signs: Vec<&str> = hunks[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c[0].as_str().unwrap())
        .collect();
    assert_eq!(signs, vec!["+", "+"]);
}

#[test]
fn diff_data_keeps_large_diffs_untruncated() {
    let old: String = (0..100).map(|i| format!("old{i}\n")).collect();
    let new: String = (0..100).map(|i| format!("new{i}\n")).collect();
    let v: serde_json::Value =
        serde_json::from_str(&serialize_diff_data(Some(&old), &new, "big.txt")).unwrap();
    assert_eq!(v["additions"], 100);
    assert_eq!(v["deletions"], 100);
    // 全量保留：整文件替换无相同行 → 无上下文，100 删 + 100 增
    let total: usize = v["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["changes"].as_array().unwrap().len())
        .sum();
    assert_eq!(total, 200);
}

#[test]
fn diff_data_identical_content_yields_no_hunks() {
    let v: serde_json::Value =
        serde_json::from_str(&serialize_diff_data(Some("a\nb\n"), "a\nb\n", "same.txt")).unwrap();
    assert_eq!(v["additions"], 0);
    assert_eq!(v["deletions"], 0);
    assert_eq!(v["hunks"].as_array().unwrap().len(), 0);
}

// ── read 工具图片附件 ────────────────────────────────

fn png_bytes() -> Vec<u8> {
    let mut v = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(b"fake-png-payload");
    v
}

#[tokio::test]
async fn read_image_file_returns_attachment() {
    let dir = tmp_workdir("read_image");
    let path = dir.join("pic.png");
    let bytes = png_bytes();
    fs::write(&path, &bytes).unwrap();

    let args = json!({ "file_path": path.display().to_string() }).to_string();
    let wd = dir.display().to_string();
    let exec = ReadTool
        .execute_async_with_display(&args, &wd)
        .await
        .unwrap();
    assert_eq!(exec.attachments.len(), 1);
    assert_eq!(exec.attachments[0].mime, "image/png");
    assert_eq!(exec.attachments[0].name.as_deref(), Some("pic.png"));
    assert!(
        exec.attachments[0]
            .data_url
            .starts_with("data:image/png;base64,")
    );
    assert!(exec.content.contains("Image read successfully"));
    assert!(exec.content.contains("<type>image</type>"));

    // execute_async 通道只回文本（附件经 execute_async_with_display 流转）
    let text = ReadTool.execute_async(&args, &wd).await.unwrap();
    assert!(text.contains("Image read successfully"));

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn read_text_file_has_no_attachments() {
    let dir = tmp_workdir("read_text");
    let path = dir.join("note.txt");
    fs::write(&path, "hello\nworld\n").unwrap();

    let args = json!({ "file_path": path.display().to_string() }).to_string();
    let exec = ReadTool
        .execute_async_with_display(&args, &dir.display().to_string())
        .await
        .unwrap();
    assert!(exec.attachments.is_empty());
    assert!(exec.content.contains("hello"));

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn read_fake_image_extension_sniffs_real_content() {
    // 扩展名为 .png 但内容是文本 → 不当图片处理（魔数优先）
    let dir = tmp_workdir("read_fake");
    let path = dir.join("fake.png");
    fs::write(&path, "just text").unwrap();

    let args = json!({ "file_path": path.display().to_string() }).to_string();
    let exec = ReadTool
        .execute_async_with_display(&args, &dir.display().to_string())
        .await
        .unwrap();
    assert!(exec.attachments.is_empty());
    assert!(exec.content.contains("just text"));

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn read_oversized_image_rejected_without_attachment() {
    let dir = tmp_workdir("read_oversized");
    let path = dir.join("big.png");
    let mut data = png_bytes();
    data.resize(crate::agent::media::MAX_IMAGE_BYTES + 1, 0);
    fs::write(&path, &data).unwrap();

    let args = json!({ "file_path": path.display().to_string() }).to_string();
    let exec = ReadTool
        .execute_async_with_display(&args, &dir.display().to_string())
        .await
        .unwrap();
    assert!(exec.attachments.is_empty());
    assert!(exec.content.contains("too large"));
    assert!(exec.content.contains("10.0 MB"));

    let _ = fs::remove_dir_all(&dir);
}

/// BashTool::extract_permission 端到端语义（扫描细节见 bash_scan 模块测试）。
fn bash_extract(command: &str, work_dir: &str) -> Vec<PermissionRequest> {
    BashTool.extract_permission(&json!({ "command_string": command }).to_string(), work_dir)
}

#[test]
fn bash_inside_workdir_single_bash_request() {
    let wd = tmp_workdir("inside");
    let wd_str = wd.display().to_string();
    // 普通命令 / 目录内文件 / 目录内写操作：单个 bash 请求，无 force_ask
    for cmd in ["ls -la", "git status", "cat src/main.rs", "rm -rf ./target"] {
        let reqs = bash_extract(cmd, &wd_str);
        assert_eq!(reqs.len(), 1, "expected single request for {cmd:?}");
        assert_eq!(reqs[0].permission, "bash");
        assert!(!reqs[0].force_ask, "expected no force_ask for {cmd:?}");
    }
}

#[test]
fn bash_outside_appends_external_request_after_bash_request() {
    let wd = tmp_workdir("outside");
    let wd_str = wd.display().to_string();
    let reqs = bash_extract("cat ../secrets.txt", &wd_str);
    // bash 请求在前（命令规则先评估），external_directory 请求在后
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].permission, "bash");
    assert_eq!(reqs[1].permission, "external_directory");
    // 绝对路径外部文件
    assert_eq!(bash_extract("cat /etc/hosts", &wd_str).len(), 2);
    // 非文件命令不受文件路径检查影响（默认放行，走 bash 规则）
    assert_eq!(bash_extract("npm install", &wd_str).len(), 1);
    assert_eq!(bash_extract("curl https://example.com", &wd_str).len(), 1);
}

#[test]
fn bash_executor_flags_force_ask() {
    let wd = tmp_workdir("executor_flag");
    let wd_str = wd.display().to_string();
    // sudo / 解释器无法静态分析 → force_ask（内置默认放行不豁免）
    for cmd in [
        "sudo systemctl restart nginx",
        "sh -c 'echo hi'",
        "python script.py",
    ] {
        let reqs = bash_extract(cmd, &wd_str);
        assert_eq!(reqs.len(), 1, "expected single request for {cmd:?}");
        assert!(reqs[0].force_ask, "expected force_ask for {cmd:?}");
    }
}

#[test]
fn bash_skips_flags_and_dynamic_args() {
    let wd = tmp_workdir("flags");
    let wd_str = wd.display().to_string();
    // -l 标志、chmod +x 模式跳过；已知变量（$HOME）展开后仍被检查
    assert_eq!(bash_extract("cat -n /etc/hosts", &wd_str).len(), 2);
    // ls 不参与文件路径检查
    assert_eq!(bash_extract("ls -l /etc", &wd_str).len(), 1);
    assert_eq!(bash_extract("chmod +x script.sh", &wd_str).len(), 1);
    assert_eq!(bash_extract("cat $HOME/.ssh/id_rsa", &wd_str).len(), 2);
    assert_eq!(bash_extract("cat ~/.ssh/id_rsa", &wd_str).len(), 2);
}

#[test]
fn bash_workdir_param_outside_wins() {
    let wd = tmp_workdir("wdparam");
    let wd_str = wd.display().to_string();
    let outside = wd.parent().unwrap().join("elsewhere");
    let args = json!({
        "command_string": "ls",
        "workdir": outside.display().to_string()
    })
    .to_string();
    let reqs = BashTool.extract_permission(&args, &wd_str);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].permission, "external_directory");
}

#[test]
fn normalize_lexical_resolves_parent_dirs() {
    let base = tmp_workdir("norm");
    let joined = Path::new(&base).join("../outside_dir");
    assert_eq!(
        normalize_lexical(&joined),
        base.parent().unwrap().join("outside_dir")
    );
}

#[test]
fn plan_mode_denies_bash_write_commands() {
    let deny = |args: &str| plan_mode_bash_denial(args);
    // 写操作 → 拒绝
    assert!(deny(r#"{"command_string":"rm -rf target"}"#).is_some());
    assert!(deny(r#"{"command_string":"git commit -m x"}"#).is_some());
    assert!(deny(r#"{"command_string":"echo hi > f"}"#).is_some());
    // 只读操作 → 放行
    assert!(deny(r#"{"command_string":"git status"}"#).is_none());
    assert!(deny(r#"{"command_string":"git diff HEAD"}"#).is_none());
    assert!(deny(r#"{"command_string":"ls -la"}"#).is_none());
    // 坏 JSON / 缺失参数 → 放行（由执行层报错）
    assert!(deny("not json").is_none());
    assert!(deny(r#"{}"#).is_none());
    // 拒绝信息包含命令原文，便于模型理解
    let reason = deny(r#"{"command_string":"rm -rf target"}"#).unwrap();
    assert!(reason.contains("rm -rf target"));
    assert!(reason.contains("plan mode"));
}

#[test]
fn decode_output_passthrough_utf8() {
    // 纯 ASCII 与合法 UTF-8（含中文）原样返回
    assert_eq!(decode_output(b"hello world"), "hello world");
    let text = "中文输出 cargo build";
    assert_eq!(decode_output(text.as_bytes()), text);
}

#[test]
fn decode_output_restores_gbk_text() {
    // powershell.exe 在中文 Windows 上按 GBK 输出错误记录，应正确还原
    let text = "拒绝访问。 (os error 5)\n所在位置 行:1 字符: 247";
    let (bytes, _, _) = encoding_rs::GBK.encode(text);
    assert_eq!(decode_output(&bytes), text);
}

#[test]
fn decode_output_prefers_utf8_on_error_tie() {
    // UTF-8 主体 + 少量坏字节：错误数打平（或 UTF-8 更少）时不应整体转 GBK
    let mut bytes = "中文输出 cargo build".as_bytes().to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    let decoded = decode_output(&bytes);
    assert!(decoded.starts_with("中文输出 cargo build"));
    assert!(decoded.contains('\u{FFFD}'));
}

#[cfg(windows)]
#[test]
fn windows_shell_program_resolves() {
    // PATH 探测不 panic，且返回两个候选之一
    let shell = windows_shell_program();
    assert!(shell == "pwsh.exe" || shell == "powershell.exe");
}

#[cfg(windows)]
#[test]
fn wrap_windows_command_injects_prologue() {
    let raw = "npm run build 2>&1 | Select-Object -Last 15";
    let wrapped = wrap_windows_command(raw);
    assert!(wrapped.starts_with("try {"), "missing try guard: {wrapped}");
    assert!(wrapped.contains("[Console]::OutputEncoding"), "{wrapped}");
    assert!(wrapped.contains("$OutputEncoding"), "{wrapped}");
    assert!(
        wrapped.ends_with(raw),
        "user command must come last: {wrapped}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn bash_utf8_prologue_preserves_unicode_output() {
    // 回归：中文 Windows 上 PS 5.1 以 GBK 双重转码原生命令输出，
    // 非 GBK 字符（✓）在 PS 层即被替换为 '?'，GBK 启发式解码无法还原。
    // prologue 强制 UTF-8 后中文与特殊符号必须原样保留。
    let out = run_shell_command(
        windows_shell_program(),
        windows_shell_command_args("Write-Output '中文✓'"),
        None,
        Some(30),
    )
    .await
    .expect("shell run");
    assert!(out.contains("中文"), "missing CJK text: {out}");
    assert!(out.contains('✓'), "mojibake regression: {out}");
}

#[cfg(windows)]
#[test]
fn bash_description_injects_shell_description() {
    let desc = BashTool.description();
    // 占位符已被替换为带版本号的 shell 描述
    assert!(!desc.contains("{SHELL_DESC}"));
    assert!(desc.contains("PowerShell"));
    assert!(desc.contains("(pwsh.exe") || desc.contains("(powershell.exe"));
}

#[cfg(windows)]
fn slow_output_command() -> (&'static str, Vec<String>) {
    (
        windows_shell_program(),
        windows_shell_command_args("Write-Output 'partial-hello'; Start-Sleep -Seconds 30"),
    )
}

#[cfg(not(windows))]
fn slow_output_command() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec!["-c".to_string(), "echo partial-hello; sleep 30".to_string()],
    )
}

#[tokio::test]
async fn bash_timeout_returns_partial_output() {
    // 超时后已产生的输出必须随超时标记一并返回（不再被 wait_with_output 的
    // 全有或全无语义丢弃），避免模型误判失败后反复重试
    let (program, args) = slow_output_command();
    let out = run_shell_command(program, args, None, Some(2))
        .await
        .expect("timeout path returns Ok");
    assert!(out.contains("timed out"), "missing marker: {out}");
    assert!(
        out.contains("partial-hello"),
        "missing partial output: {out}"
    );
}

#[tokio::test]
async fn bash_normal_exit_returns_stdout() {
    // 正常完成路径行为不变：无超时标记、输出完整
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        windows_shell_command_args("Write-Output 'quick-hello'"),
    );
    #[cfg(not(windows))]
    let (program, args) = (
        "bash",
        vec!["-c".to_string(), "echo quick-hello".to_string()],
    );
    let out = run_shell_command(program, args, None, Some(30))
        .await
        .expect("normal exit");
    assert!(out.contains("quick-hello"), "missing output: {out}");
    assert!(!out.contains("timed out"), "unexpected marker: {out}");
}

#[tokio::test]
async fn bash_grandchild_holding_pipe_returns_on_exit() {
    // 回归：孙进程继承并持有管道写端（playwright-cli daemon 等常驻进程的场景）。
    // 旧实现 wait_with_output 死等 EOF → 挂满超时；新实现以进程退出为准 + 限时排空。
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        // cmd 立即退出，ping 继承管道写端并存活 30s
        windows_shell_command_args(
            "Write-Output 'grand-child-test'; cmd /c \"start /b ping -n 30 127.0.0.1\"",
        ),
    );
    #[cfg(not(windows))]
    let (program, args) = (
        "bash",
        vec![
            "-c".to_string(),
            // shell 立即退出，后台 sleep 持有管道写端
            "echo grand-child-test; sleep 30 &".to_string(),
        ],
    );
    let started = std::time::Instant::now();
    let out = run_shell_command(program, args, None, Some(30))
        .await
        .expect("returns after process exit");
    let elapsed = started.elapsed();
    assert!(out.contains("grand-child-test"), "missing output: {out}");
    assert!(!out.contains("timed out"), "unexpected marker: {out}");
    assert!(
        elapsed < Duration::from_secs(10),
        "should return on exit, not wait for EOF: {elapsed:?}"
    );
}

#[tokio::test]
async fn drain_channel_stops_at_hard_cap_despite_chatty_writer() {
    // 回归：持续低速写入会无限重置空闲窗口（chatty daemon 场景），
    // 必须被硬性总上限截停，否则排空永不结束、命令永不返回。
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let writer = tokio::spawn(async move {
        // 每 100ms 写一块（< 500ms 空闲窗口），持续 20s 远超 3s 硬性上限
        for _ in 0..200 {
            if tx.send(vec![b'x'; 16]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let mut buf = Vec::new();
    let started = std::time::Instant::now();
    drain_channel_with_window(&mut rx, &mut buf, Duration::from_millis(500)).await;
    let elapsed = started.elapsed();
    writer.abort();
    assert!(
        elapsed < DRAIN_HARD_CAP + Duration::from_millis(1500),
        "drain exceeded hard cap: {elapsed:?}"
    );
    assert!(!buf.is_empty(), "should have drained some chunks");
}

#[tokio::test]
async fn bash_nonzero_exit_combines_stdout_and_stderr() {
    // 失败命令：退出码非 0 → 合并 stdout 与 stderr（combine_output 路径）
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        windows_shell_command_args(
            "Write-Output 'out-part'; [Console]::Error.Write('err-part'); exit 3",
        ),
    );
    #[cfg(not(windows))]
    let (program, args) = (
        "bash",
        vec![
            "-c".to_string(),
            "echo out-part; echo err-part >&2; exit 3".to_string(),
        ],
    );
    let out = run_shell_command(program, args, None, Some(30))
        .await
        .expect("nonzero exit still returns Ok");
    assert!(out.contains("out-part"), "missing stdout: {out}");
    assert!(out.contains("err-part"), "missing stderr: {out}");
}

#[tokio::test]
async fn bash_large_output_truncated() {
    // 超过 10000 字符 → 截断标记 + 保留前 10000 字符
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        windows_shell_command_args("Write-Output ('x'*12000)"),
    );
    #[cfg(not(windows))]
    let (program, args) = (
        "bash",
        vec![
            "-c".to_string(),
            "head -c 12000 /dev/zero | tr '\\0' 'x'".to_string(),
        ],
    );
    let out = run_shell_command(program, args, None, Some(60))
        .await
        .expect("large output command");
    assert!(out.contains("Output truncated"), "missing marker: {out}");
    assert!(
        out.contains("showing 10000 of"),
        "missing counts: {}",
        out.chars().rev().take(200).collect::<String>()
    );
}

#[tokio::test]
async fn bash_workdir_is_respected() {
    // workdir 参数：命令在指定目录执行
    let dir = std::env::temp_dir().join("hailux_bash_wd_test");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        windows_shell_command_args("Get-Location"),
    );
    #[cfg(not(windows))]
    let (program, args) = ("bash", vec!["-c".to_string(), "pwd".to_string()]);
    let out = run_shell_command(program, args, Some(dir.display().to_string()), Some(30))
        .await
        .expect("workdir command");
    assert!(
        out.contains("hailux_bash_wd_test"),
        "expected workdir in output: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bash_timeout_with_no_output_returns_marker_only() {
    // 超时且无任何输出 → 仅返回超时标记，不携带空 body
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        windows_shell_command_args("Start-Sleep -Seconds 30"),
    );
    #[cfg(not(windows))]
    let (program, args) = ("bash", vec!["-c".to_string(), "sleep 30".to_string()]);
    let out = run_shell_command(program, args, None, Some(2))
        .await
        .expect("timeout path returns Ok");
    assert_eq!(
        out.trim(),
        "[Command timed out (2s), process tree terminated]",
        "unexpected body: {out}"
    );
}

#[tokio::test]
async fn bash_tool_rejects_invalid_json() {
    let err = BashTool.execute_async("not json", ".").await.unwrap_err();
    assert!(
        err.message.contains("Invalid JSON parameter"),
        "unexpected error: {}",
        err.message
    );
}

#[tokio::test]
async fn bash_tool_rejects_missing_command() {
    let err = BashTool.execute_async("{}", ".").await.unwrap_err();
    assert!(
        err.message.contains("Missing 'command_string'"),
        "unexpected error: {}",
        err.message
    );
}

#[tokio::test]
async fn bash_tool_end_to_end_json_roundtrip() {
    // BashTool::execute_async 端到端：JSON 参数解析（含 timeout 字段）→ 执行 → 返回
    #[cfg(windows)]
    let arguments = r#"{"command_string":"Write-Output 'tool-e2e'","timeout":30}"#.to_string();
    #[cfg(not(windows))]
    let arguments = r#"{"command_string":"echo tool-e2e","timeout":30}"#.to_string();
    let out = BashTool
        .execute_async(&arguments, ".")
        .await
        .expect("e2e run");
    assert!(out.contains("tool-e2e"), "missing output: {out}");
    assert!(!out.contains("timed out"), "unexpected marker: {out}");
}

#[tokio::test]
async fn bash_tool_defaults_to_work_dir_without_workdir_arg() {
    // 未传 workdir 时命令必须落在会话工作目录，而不是进程 cwd
    let dir = test_tmp_dir("hailux_bash_workdir_default");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let wd = dir.display().to_string();
    #[cfg(windows)]
    let arguments = r#"{"command_string":"Write-Output (Get-Location).Path"}"#.to_string();
    #[cfg(not(windows))]
    let arguments = r#"{"command_string":"pwd"}"#.to_string();
    let out = BashTool
        .execute_async(&arguments, &wd)
        .await
        .expect("bash run");
    // 验证输出路径位于工作目录下（Windows canonicalize 可能带 \\?\ 前缀，宽松匹配）
    let tail = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    assert!(
        !tail.is_empty() && out.contains(&tail),
        "cwd mismatch: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn grep_tool_caps_at_max_matches() {
    // 超过 MAX_MATCHES → 恰好 100 条命中 + 截断标记（外层遍历提前终止，输出不变）
    let dir = test_tmp_dir("hailux_grep_truncate_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let content: String = (0..150).map(|i| format!("needle line {i}\n")).collect();
    std::fs::write(dir.join("big.txt"), &content).expect("write file");
    let arguments =
        serde_json::json!({ "pattern": "needle", "path": dir.display().to_string() }).to_string();
    let out = GrepTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("grep run");
    let match_lines = out
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .count();
    assert_eq!(match_lines, 100, "should cap at 100 matches: {out}");
    assert!(out.contains("Too many results"), "missing marker: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn grep_tool_no_matches_returns_marker() {
    let dir = test_tmp_dir("hailux_grep_empty_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("a.txt"), "hello world\n").expect("write file");
    let arguments =
        serde_json::json!({ "pattern": "zzz_not_there", "path": dir.display().to_string() })
            .to_string();
    let out = GrepTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("grep run");
    assert_eq!(out, "No matches found");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn grep_tool_defaults_to_work_dir() {
    // 缺省 path（"."）→ 按会话工作目录搜索，而非进程 cwd
    let dir = test_tmp_dir("hailux_grep_default_path");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("marker.txt"), "unique_marker_xyz\n").expect("write file");
    let arguments = serde_json::json!({ "pattern": "unique_marker_xyz" }).to_string();
    let out = GrepTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("grep run");
    assert!(out.contains("marker.txt"), "missing match: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn glob_tool_filters_by_pattern() {
    let dir = test_tmp_dir("hailux_glob_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").expect("write file");
    std::fs::write(dir.join("b.txt"), "text\n").expect("write file");
    let arguments =
        serde_json::json!({ "pattern": "*.rs", "path": dir.display().to_string() }).to_string();
    let out = GlobTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("glob run");
    assert!(out.contains("a.rs"), "missing match: {out}");
    assert!(!out.contains("b.txt"), "unexpected match: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn glob_tool_defaults_to_work_dir() {
    // 缺省 path（"."）→ 按会话工作目录搜索，而非进程 cwd
    let dir = test_tmp_dir("hailux_glob_default_path");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("only_here.rs"), "fn x() {}\n").expect("write file");
    let arguments = serde_json::json!({ "pattern": "*.rs" }).to_string();
    let out = GlobTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("glob run");
    assert!(out.contains("only_here.rs"), "missing match: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_tool_path_resolves_relative_against_work_dir() {
    #[cfg(windows)]
    {
        assert_eq!(
            resolve_tool_path("src/main.rs", r"D:\proj"),
            r"D:\proj\src\main.rs"
        );
        assert_eq!(resolve_tool_path(".", r"D:\proj"), r"D:\proj");
        assert_eq!(
            resolve_tool_path(r"D:\other\a.rs", r"D:\proj"),
            r"D:\other\a.rs"
        );
        // 盘符相对路径（D:foo）不拼接，避免产生错误组合
        assert_eq!(resolve_tool_path(r"D:foo", r"D:\proj"), r"D:foo");
        // 实际运行时 work_dir 为 canonicalize 结果，带 \\?\ verbatim 前缀
        assert_eq!(resolve_tool_path("src", r"\\?\D:\proj"), r"\\?\D:\proj\src");
    }
    #[cfg(not(windows))]
    {
        assert_eq!(
            resolve_tool_path("src/main.rs", "/proj"),
            "/proj/src/main.rs"
        );
        assert_eq!(resolve_tool_path(".", "/proj"), "/proj");
        assert_eq!(resolve_tool_path("/other/a.rs", "/proj"), "/other/a.rs");
    }
}

/// 测试临时目录：放在 target/ 下（沙箱/受限环境常禁写 %TEMP%，target 必可写）
fn test_tmp_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp")
        .join(name)
}
