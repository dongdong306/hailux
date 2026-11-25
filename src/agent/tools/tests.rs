use super::diff::serialize_diff_data;
use super::shell::{DRAIN_HARD_CAP, drain_channel_with_window};
use super::shell::{decode_output, run_shell_command, strip_ansi};
#[cfg(windows)]
use super::shell::{windows_shell_command_args, windows_shell_program, wrap_windows_command};
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
    assert_eq!(changes.len(), 7); // 2 leading context + 1 del + 1 add + 3 trailing context
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
    // Kept in full: whole-file replacement has no common lines → no context, 100 dels + 100 adds
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

// ── read tool image attachments ────────────────────────────────

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

    // The execute_async channel returns text only (attachments flow through
    // execute_async_with_display)
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
    // .png extension but text content → not treated as an image (magic number wins)
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

/// End-to-end semantics of ShellTool::extract_permission (scan details are covered by the shell_scan module tests).
fn shell_extract(command: &str, work_dir: &str) -> Vec<PermissionRequest> {
    ShellTool.extract_permission(&json!({ "command_string": command }).to_string(), work_dir)
}

#[test]
fn shell_inside_workdir_single_request() {
    let wd = tmp_workdir("inside");
    let wd_str = wd.display().to_string();
    // Plain command / in-workdir file / in-workdir write: single shell request, no force_ask
    for cmd in ["ls -la", "git status", "cat src/main.rs", "rm -rf ./target"] {
        let reqs = shell_extract(cmd, &wd_str);
        assert_eq!(reqs.len(), 1, "expected single request for {cmd:?}");
        assert_eq!(reqs[0].permission, "shell");
        assert!(!reqs[0].force_ask, "expected no force_ask for {cmd:?}");
    }
}

#[test]
fn shell_outside_appends_external_request_after_shell_request() {
    let wd = tmp_workdir("outside");
    let wd_str = wd.display().to_string();
    let reqs = shell_extract("cat ../secrets.txt", &wd_str);
    // Shell request first (command rules evaluated first), external_directory second
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].permission, "shell");
    assert_eq!(reqs[1].permission, "external_directory");
    // External file with an absolute path
    assert_eq!(shell_extract("cat /etc/hosts", &wd_str).len(), 2);
    // Non-file commands bypass the file-path check (default allow, shell rules apply)
    assert_eq!(shell_extract("npm install", &wd_str).len(), 1);
    assert_eq!(shell_extract("curl https://example.com", &wd_str).len(), 1);
}

#[test]
fn shell_executor_flags_force_ask() {
    let wd = tmp_workdir("executor_flag");
    let wd_str = wd.display().to_string();
    // sudo / interpreters cannot be statically analyzed → force_ask (the
    // built-in default-allow does not exempt them)
    for cmd in [
        "sudo systemctl restart nginx",
        "sh -c 'echo hi'",
        "python script.py",
    ] {
        let reqs = shell_extract(cmd, &wd_str);
        assert_eq!(reqs.len(), 1, "expected single request for {cmd:?}");
        assert!(reqs[0].force_ask, "expected force_ask for {cmd:?}");
    }
}

#[test]
fn shell_skips_flags_and_dynamic_args() {
    let wd = tmp_workdir("flags");
    let wd_str = wd.display().to_string();
    // -l flags and chmod +x modes are skipped; known variables ($HOME) are
    // still checked after expansion
    assert_eq!(shell_extract("cat -n /etc/hosts", &wd_str).len(), 2);
    // ls is not subject to the file-path check
    assert_eq!(shell_extract("ls -l /etc", &wd_str).len(), 1);
    assert_eq!(shell_extract("chmod +x script.sh", &wd_str).len(), 1);
    assert_eq!(shell_extract("cat $HOME/.ssh/id_rsa", &wd_str).len(), 2);
    assert_eq!(shell_extract("cat ~/.ssh/id_rsa", &wd_str).len(), 2);
}

#[test]
fn shell_workdir_param_outside_wins() {
    let wd = tmp_workdir("wdparam");
    let wd_str = wd.display().to_string();
    let outside = wd.parent().unwrap().join("elsewhere");
    let args = json!({
        "command_string": "ls",
        "workdir": outside.display().to_string()
    })
    .to_string();
    let reqs = ShellTool.extract_permission(&args, &wd_str);
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
fn plan_mode_denies_shell_write_commands() {
    let deny = |args: &str| plan_mode_shell_denial(args);
    // Write operation → deny
    assert!(deny(r#"{"command_string":"rm -rf target"}"#).is_some());
    assert!(deny(r#"{"command_string":"git commit -m x"}"#).is_some());
    assert!(deny(r#"{"command_string":"echo hi > f"}"#).is_some());
    // Read-only operation → allow
    assert!(deny(r#"{"command_string":"git status"}"#).is_none());
    assert!(deny(r#"{"command_string":"git diff HEAD"}"#).is_none());
    assert!(deny(r#"{"command_string":"ls -la"}"#).is_none());
    // Bad JSON / missing arguments → allow (the execution layer reports the error)
    assert!(deny("not json").is_none());
    assert!(deny(r#"{}"#).is_none());
    // The denial message includes the raw command so the model can understand it
    let reason = deny(r#"{"command_string":"rm -rf target"}"#).unwrap();
    assert!(reason.contains("rm -rf target"));
    assert!(reason.contains("plan mode"));
}

#[test]
fn decode_output_passthrough_utf8() {
    // Pure ASCII and valid UTF-8 (including CJK) are returned as-is
    assert_eq!(decode_output(b"hello world"), "hello world");
    let text = "中文输出 cargo build";
    assert_eq!(decode_output(text.as_bytes()), text);
}

#[test]
fn decode_output_restores_gbk_text() {
    // powershell.exe emits error records as GBK on Chinese Windows; decoding
    // must restore them correctly
    let text = "拒绝访问。 (os error 5)\n所在位置 行:1 字符: 247";
    let (bytes, _, _) = encoding_rs::GBK.encode(text);
    assert_eq!(decode_output(&bytes), text);
}

#[test]
fn decode_output_prefers_utf8_on_error_tie() {
    // Mostly UTF-8 + a few bad bytes: on an error tie (or UTF-8 having fewer
    // errors) the text must not be converted wholesale to GBK
    let mut bytes = "中文输出 cargo build".as_bytes().to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    let decoded = decode_output(&bytes);
    assert!(decoded.starts_with("中文输出 cargo build"));
    assert!(decoded.contains('\u{FFFD}'));
}

#[test]
fn strip_ansi_removes_csi_sequences() {
    // Color / cursor / erase sequences are removed, text preserved
    assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[39m"), "ok");
    assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m"), "error");
    assert_eq!(strip_ansi("\u{1b}[2K\u{1b}[1Gdone"), "done");
    assert_eq!(strip_ansi("a\u{1b}[?25lb"), "ab");
}

#[test]
fn strip_ansi_removes_osc_and_string_sequences() {
    // OSC (window titles) and DCS payloads are removed together with the
    // sequence, for both BEL and ESC \ terminators
    assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
    assert_eq!(strip_ansi("\u{1b}]0;title\u{1b}\\text"), "text");
    assert_eq!(strip_ansi("\u{1b}P+q544e\u{1b}\\done"), "done");
}

#[test]
fn strip_ansi_keeps_plain_text_identical() {
    // Regression guard: text without ESC bytes must pass through unchanged
    // (CJK, symbols, newlines — including sequences that merely look like text)
    let text = "中文输出 cargo build ✓ 100%\n\t[a] (b) {c}";
    assert_eq!(strip_ansi(text), text);
    assert_eq!(strip_ansi("hello world"), "hello world");
}

#[test]
fn strip_ansi_drops_incomplete_tail() {
    // A sequence truncated at buffer end must not leak its bytes
    assert_eq!(strip_ansi("ok\u{1b}[32"), "ok");
    assert_eq!(strip_ansi("ok\u{1b}]0;ti"), "ok");
    assert_eq!(strip_ansi("ok\u{1b}"), "ok");
}

#[test]
fn strip_ansi_removes_escapes_with_intermediates() {
    // ESC + intermediate bytes + final byte (charset selection etc.) is a
    // single sequence: the final byte must not leak into the text
    assert_eq!(strip_ansi("\u{1b}(Bplain"), "plain");
    assert_eq!(strip_ansi("\u{1b}%Gutf8"), "utf8");
    assert_eq!(strip_ansi("a\u{1b}/Fb"), "ab");
}

#[test]
fn strip_ansi_treats_esc_esc_as_st_terminator() {
    // ESC ESC inside a string sequence: the second ESC belongs to the ST
    // terminator (`ESC ESC \`), it must not start a new sequence
    assert_eq!(strip_ansi("\u{1b}]0;t\u{1b}\u{1b}\\x"), "x");
}

#[cfg(windows)]
#[test]
fn windows_shell_program_resolves() {
    // PATH probing must not panic and must return one of the two candidates
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
async fn shell_utf8_prologue_preserves_unicode_output() {
    // Regression: on Chinese Windows PS 5.1 double-transcodes native command
    // output as GBK; non-GBK characters (✓) are replaced with '?' at the PS
    // layer, and GBK heuristic decoding cannot restore them.
    // With the prologue forcing UTF-8, CJK text and special symbols must be
    // preserved as-is.
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
#[tokio::test]
async fn shell_strips_ansi_color_output() {
    // Regression: colored tool output (npm build etc.) must not leak escape
    // sequences into the result; the visible text itself must survive
    let script = "Write-Output ([char]27 + '[32mgreen中文' + [char]27 + '[0m plain')";
    let out = run_shell_command(
        windows_shell_program(),
        windows_shell_command_args(script),
        None,
        Some(30),
    )
    .await
    .expect("shell run");
    assert!(!out.contains('\u{1b}'), "escape sequence leaked: {out:?}");
    assert!(out.contains("green中文"), "text lost: {out:?}");
    assert!(out.contains("plain"), "text lost: {out:?}");
}

#[cfg(windows)]
#[test]
fn shell_description_injects_shell_description() {
    let desc = ShellTool.description();
    // The placeholder has been replaced with the versioned shell description
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
async fn shell_timeout_returns_partial_output() {
    // Output already produced before the timeout must be returned together with
    // the timeout marker (no longer discarded by wait_with_output's all-or-nothing
    // semantics), so the model does not misjudge failure and retry repeatedly
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
async fn shell_normal_exit_returns_stdout() {
    // The normal-completion path is unchanged: no timeout marker, complete output
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
async fn shell_grandchild_holding_pipe_returns_on_exit() {
    // Regression: a grandchild inherits and holds the pipe write end (resident
    // processes like the playwright-cli daemon). The old wait_with_output waited
    // forever for EOF → hung until timeout; the new implementation returns on
    // process exit + a time-limited drain.
    #[cfg(windows)]
    let (program, args) = (
        windows_shell_program(),
        // cmd exits immediately; ping inherits the pipe write end and lives for 30s
        windows_shell_command_args(
            "Write-Output 'grand-child-test'; cmd /c \"start /b ping -n 30 127.0.0.1\"",
        ),
    );
    #[cfg(not(windows))]
    let (program, args) = (
        "bash",
        vec![
            "-c".to_string(),
            // shell exits immediately; the background sleep holds the pipe write end
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
    // Regression: continuous slow writes reset the idle window indefinitely
    // (chatty daemon scenario); they must be cut off by the hard overall cap,
    // otherwise draining never ends and the command never returns.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let writer = tokio::spawn(async move {
        // Writes one chunk every 100ms (< the 500ms idle window) for 20s, exceeding the 3s cap
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
async fn shell_nonzero_exit_combines_stdout_and_stderr() {
    // Failing command: nonzero exit code → stdout and stderr combined (combine_output path)
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
async fn shell_large_output_truncated() {
    // Over 10000 characters → truncation marker + first 10000 characters kept
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
async fn shell_workdir_is_respected() {
    // workdir argument: the command runs in the given directory
    let dir = std::env::temp_dir().join("hailux_shell_wd_test");
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
        out.contains("hailux_shell_wd_test"),
        "expected workdir in output: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn shell_timeout_with_no_output_returns_marker_only() {
    // Timeout with no output at all → only the timeout marker is returned, without an empty body
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
async fn shell_tool_rejects_invalid_json() {
    let err = ShellTool.execute_async("not json", ".").await.unwrap_err();
    assert!(
        err.message.contains("Invalid JSON parameter"),
        "unexpected error: {}",
        err.message
    );
}

#[tokio::test]
async fn shell_tool_rejects_missing_command() {
    let err = ShellTool.execute_async("{}", ".").await.unwrap_err();
    assert!(
        err.message.contains("Missing 'command_string'"),
        "unexpected error: {}",
        err.message
    );
}

#[tokio::test]
async fn shell_tool_end_to_end_json_roundtrip() {
    // ShellTool::execute_async end to end: JSON argument parsing (incl. timeout field) → execution → result
    #[cfg(windows)]
    let arguments = r#"{"command_string":"Write-Output 'tool-e2e'","timeout":30}"#.to_string();
    #[cfg(not(windows))]
    let arguments = r#"{"command_string":"echo tool-e2e","timeout":30}"#.to_string();
    let out = ShellTool
        .execute_async(&arguments, ".")
        .await
        .expect("e2e run");
    assert!(out.contains("tool-e2e"), "missing output: {out}");
    assert!(!out.contains("timed out"), "unexpected marker: {out}");
}

#[tokio::test]
async fn shell_tool_defaults_to_work_dir_without_workdir_arg() {
    // Without a workdir argument the command must land in the session working
    // directory, not the process cwd
    let dir = test_tmp_dir("hailux_shell_workdir_default");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let wd = dir.display().to_string();
    #[cfg(windows)]
    let arguments = r#"{"command_string":"Write-Output (Get-Location).Path"}"#.to_string();
    #[cfg(not(windows))]
    let arguments = r#"{"command_string":"pwd"}"#.to_string();
    let out = ShellTool
        .execute_async(&arguments, &wd)
        .await
        .expect("shell run");
    // Verify the output path is under the working directory (Windows
    // canonicalize may add a \\?\ prefix; loose matching)
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
    // Over MAX_MATCHES → exactly 100 hits + truncation marker (the outer walk
    // terminates early; output unchanged)
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
async fn grep_skips_env_files_in_directory_search() {
    // Regression: directory-level search used to return *.env.* contents
    // directly without asking (bypassing the read gate)
    let dir = test_tmp_dir("hailux_grep_env_skip");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for name in ["a.rs", "k.env", "b.env.local", ".env.example"] {
        std::fs::write(dir.join(name), "probe_secret\n").expect("write file");
    }
    let arguments = serde_json::json!({
        "pattern": "probe_secret",
        "path": dir.display().to_string()
    })
    .to_string();
    let out = GrepTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("grep run");
    assert!(out.contains("a.rs"), "missing normal match: {out}");
    assert!(
        out.contains(".env.example"),
        "example file must stay searchable: {out}"
    );
    assert!(!out.contains("k.env:"), "env file must be skipped: {out}");
    assert!(
        !out.contains("b.env.local"),
        "env file must be skipped: {out}"
    );
    assert!(out.contains("skipped 2 env-type"), "missing note: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn grep_explicit_env_path_searches_root_file() {
    // path pointing directly at an env file = explicit target (extract_permission
    // already issued the read ask); the executor must not skip the traversal
    // root itself
    let dir = test_tmp_dir("hailux_grep_env_explicit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let env = dir.join("k.env");
    std::fs::write(&env, "probe_secret\n").expect("write file");
    let arguments = serde_json::json!({
        "pattern": "probe_secret",
        "path": env.display().to_string()
    })
    .to_string();
    let out = GrepTool
        .execute_async(&arguments, &dir.display().to_string())
        .await
        .expect("grep run");
    assert!(
        out.contains("probe_secret"),
        "root env file must be searched: {out}"
    );
    assert!(!out.contains("skipped"), "unexpected skip note: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_extract_permission_adds_read_gate_for_env_file() {
    let wd = tmp_workdir("grep_env_perm");
    let wd_str = wd.display().to_string();
    // Directory search: grep request only
    let reqs = GrepTool.extract_permission(
        &serde_json::json!({ "pattern": "x", "path": "src" }).to_string(),
        &wd_str,
    );
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].permission, "grep");
    // Explicit env file: a read request is appended (hits the built-in *.env ask rule)
    let reqs = GrepTool.extract_permission(
        &serde_json::json!({ "pattern": "x", "path": ".env" }).to_string(),
        &wd_str,
    );
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].permission, "read");
    assert!(reqs[1].patterns[0].ends_with(".env"));
    // Exempt files get no appended request
    let reqs = GrepTool.extract_permission(
        &serde_json::json!({ "pattern": "x", "path": ".env.example" }).to_string(),
        &wd_str,
    );
    assert_eq!(reqs.len(), 1);
}

#[tokio::test]
async fn grep_tool_defaults_to_work_dir() {
    // Default path (".") → search the session working directory, not the process cwd
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
    // Default path (".") → search the session working directory, not the process cwd
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

#[tokio::test]
async fn glob_tool_matches_relative_subdir_pattern() {
    // Regression: patterns with a relative directory prefix like `src/tui/*.rs`
    // never matched absolute entry paths (the prefix could not be consumed);
    // matching must also run against the path relative to the traversal root
    let dir = test_tmp_dir("hailux_glob_relative_pattern");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/tui")).expect("create dirs");
    std::fs::write(dir.join("src/tui/mod.rs"), "fn t() {}\n").expect("write file");
    std::fs::write(dir.join("src/main.rs"), "fn m() {}\n").expect("write file");
    let args = |pattern: &str| {
        serde_json::json!({ "pattern": pattern, "path": dir.display().to_string() }).to_string()
    };
    let out = GlobTool
        .execute_async(&args("src/tui/*.rs"), &dir.display().to_string())
        .await
        .expect("glob run");
    assert!(out.contains("mod.rs"), "missing match: {out}");
    assert!(!out.contains("main.rs"), "unexpected match: {out}");

    let out = GlobTool
        .execute_async(&args("src/**/*.rs"), &dir.display().to_string())
        .await
        .expect("glob run");
    assert!(out.contains("mod.rs"), "missing match: {out}");
    assert!(out.contains("main.rs"), "missing match: {out}");

    // With a subdirectory as the traversal root, relative patterns anchor at the root too
    let out = GlobTool
        .execute_async(&args("*.rs"), &dir.join("src/tui").display().to_string())
        .await
        .expect("glob run");
    assert!(out.contains("mod.rs"), "missing match: {out}");
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
        // Drive-relative paths (D:foo) are not joined, avoiding a wrong combination
        assert_eq!(resolve_tool_path(r"D:foo", r"D:\proj"), r"D:foo");
        // At runtime work_dir is a canonicalize result carrying the \\?\ verbatim prefix
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

/// Test temp directory: placed under target/ (sandboxed/restricted environments
/// often forbid writes to %TEMP%; target is always writable)
fn test_tmp_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp")
        .join(name)
}
