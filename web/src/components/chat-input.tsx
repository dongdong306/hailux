// 输入区：wendao Composer 同构卡片 —— Enter 发送 / Shift+Enter 换行 / 粘贴保护 / 输入历史 / 双击 Esc 中断 / Shift+Tab Plan
// @ 文件提及补全：对齐 TUI 语义（chat_input.rs）—— `@` 前须为行首/空白、query 内不含空白；
// 选中插入 `@rel `，发送时展开为 `@绝对路径`
// / 斜杠命令补全：prompt 型（/init、自定义命令）发送原文由后端展开；
// ui 型（/compact）本地分发。其余 UI 命令（/new /sessions /skills /mcp 等）Web 端已有界面入口，不提供
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  ArrowUp,
  ClipboardList,
  Cpu,
  FileText,
  Loader2,
  Square,
  Terminal,
  Zap,
} from "lucide-react";
import { useApp } from "../store/app-store";
import type { CommandInfo } from "../runtime/types";
import { cn } from "../lib/utils";

/** 解析光标前输入中的 `@query` 片段；无有效触发时返回 null（query 可为空串） */
function fileMentionQuery(beforeCursor: string): string | null {
  const atIdx = beforeCursor.lastIndexOf("@");
  if (atIdx < 0) return null;
  const beforeAt = beforeCursor.slice(0, atIdx);
  if (beforeAt && !/\s/.test(beforeAt[beforeAt.length - 1]!)) return null;
  const query = beforeCursor.slice(atIdx + 1);
  if (/\s/.test(query)) return null;
  return query;
}

/**
 * 解析斜杠命令触发：输入以 `/` 开头且光标前无空白（正在输入首 token）时返回命令前缀（不含 `/`）。
 */
function slashCommandQuery(beforeCursor: string): string | null {
  if (!beforeCursor.startsWith("/") || /\s/.test(beforeCursor)) return null;
  return beforeCursor.slice(1);
}

interface MentionState {
  query: string;
  results: string[];
  selected: number;
  loading: boolean;
}

interface SlashState {
  query: string;
  items: CommandInfo[];
  selected: number;
}

export function ChatInput() {
  const [text, setText] = useState("");
  const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [lastEscAt, setLastEscAt] = useState(0);
  const [pastedAt, setPastedAt] = useState(0);
  const [mention, setMention] = useState<MentionState | null>(null);
  const [slash, setSlash] = useState<SlashState | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const mentionListRef = useRef<HTMLDivElement>(null);
  const slashListRef = useRef<HTMLDivElement>(null);
  const mentionTimer = useRef<number | null>(null);
  const mentionAbort = useRef<AbortController | null>(null);
  /** Esc 关闭时的触发片段（`@query`），文本再次变化前不重新打开 */
  const mentionDismissed = useRef<string | null>(null);
  /** 斜杠命令下拉 Esc 关闭片段（`/query`），语义同上 */
  const slashDismissed = useRef<string | null>(null);
  /** picker 选中记录：`@rel` 展示 → 绝对路径，发送时替换 */
  const pendingMentions = useRef(new Map<string, string>());

  const isRunning = useApp((s) => s.isRunning);
  const planMode = useApp((s) => s.planMode);
  const sendMessage = useApp((s) => s.sendMessage);
  const runCompact = useApp((s) => s.runCompact);
  const interrupt = useApp((s) => s.interrupt);
  const setPlanMode = useApp((s) => s.setPlanMode);
  const escHint = useApp((s) => s.escHint);
  const setEscHint = useApp((s) => s.setEscHint);
  const model = useApp((s) => s.models.find((m) => m.active)?.display ?? "");
  const setModelPicker = useApp((s) => s.setModelPicker);
  const yolo = useApp((s) => s.yolo);
  const setYolo = useApp((s) => s.setYolo);
  const commands = useApp((s) => s.commands);

  useEffect(() => {
    if (!escHint) return;
    const t = setTimeout(() => setEscHint(false), 5000);
    return () => clearTimeout(t);
  }, [escHint, setEscHint]);

  // 自适应高度：默认 2 行，内容超过后增高，最高 10 行（超出后滚动）
  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const s = getComputedStyle(el);
    const lineH = parseFloat(s.lineHeight) || 24;
    const pad =
      (parseFloat(s.paddingTop) || 0) + (parseFloat(s.paddingBottom) || 0);
    const min = lineH * 2 + pad;
    const max = lineH * 10 + pad;
    el.style.height = `${Math.min(max, Math.max(min, el.scrollHeight))}px`;
  }, [text]);

  // 选中项滚动可见
  useEffect(() => {
    mentionListRef.current
      ?.querySelector<HTMLElement>('[data-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [mention?.selected]);

  useEffect(() => {
    slashListRef.current
      ?.querySelector<HTMLElement>('[data-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [slash?.selected]);

  const closeMention = () => {
    if (mentionTimer.current !== null) {
      window.clearTimeout(mentionTimer.current);
      mentionTimer.current = null;
    }
    mentionAbort.current?.abort();
    mentionAbort.current = null;
    setMention(null);
  };

  const scheduleMentionSearch = (query: string) => {
    if (mentionTimer.current !== null) window.clearTimeout(mentionTimer.current);
    mentionAbort.current?.abort();
    setMention((m) =>
      m
        ? { ...m, query, loading: true }
        : { query, results: [], selected: 0, loading: true },
    );
    mentionTimer.current = window.setTimeout(async () => {
      const ctrl = new AbortController();
      mentionAbort.current = ctrl;
      try {
        const workDir = useApp.getState().workDir;
        const params = new URLSearchParams({ q: query });
        if (workDir) params.set("work_dir", workDir);
        const resp = await fetch(`/api/files?${params}`, {
          signal: ctrl.signal,
        });
        if (!resp.ok) return;
        const files = (await resp.json()) as string[];
        setMention((m) =>
          m && m.query === query
            ? files.length > 0
              ? { query, results: files.slice(0, 20), selected: 0, loading: false }
              : null
            : m,
        );
      } catch {
        // 请求被后续输入取消 —— 忽略
      }
    }, 120);
  };

  const applyMention = (rel: string) => {
    const el = textareaRef.current;
    if (!el || !mention) return;
    const cursor = el.selectionStart ?? text.length;
    const before = text.slice(0, cursor);
    const atIdx = before.lastIndexOf("@");
    if (atIdx < 0) return;
    const display = `@${rel}`;
    const workDir = useApp.getState().workDir.replace(/[\\/]+$/, "");
    pendingMentions.current.set(
      display,
      workDir ? `${workDir}/${rel}` : rel,
    );
    setText(text.slice(0, atIdx) + display + " " + text.slice(cursor));
    closeMention();
    requestAnimationFrame(() => {
      el.focus();
      const pos = atIdx + display.length + 1;
      el.selectionStart = el.selectionEnd = pos;
    });
  };

  /** 选中斜杠命令：输入框替换为 `/name `（尾随空格便于直接输入参数） */
  const applyCommand = (cmd: CommandInfo) => {
    const el = textareaRef.current;
    setText(`/${cmd.name} `);
    setSlash(null);
    slashDismissed.current = null;
    requestAnimationFrame(() => {
      el?.focus();
      const pos = cmd.name.length + 2;
      if (el) el.selectionStart = el.selectionEnd = pos;
    });
  };

  /** 发送时把 `@rel` 展示替换为 `@绝对路径`（对齐 TUI expand_file_mentions） */
  const expandMentions = (value: string) => {
    let out = value;
    for (const [display, abs] of pendingMentions.current) {
      out = out.split(display).join(`@${abs}`);
    }
    pendingMentions.current.clear();
    return out;
  };

  const submit = () => {
    if (!text.trim() || isRunning) return;
    const value = expandMentions(text.trim());

    // ui 型斜杠命令本地分发（当前仅 /compact）；prompt 型发送原文由后端展开
    const firstWord = value.split(/\s+/, 1)[0] ?? "";
    if (firstWord.startsWith("/")) {
      const cmd = useApp
        .getState()
        .commands.find((c) => c.name === firstWord.slice(1));
      if (cmd?.kind === "ui") {
        setText("");
        setHistoryIndex(null);
        setDraft("");
        if (cmd.name === "compact") runCompact().catch(() => {});
        return;
      }
    }

    setText("");
    setHistoryIndex(null);
    setDraft("");
    sendMessage(value);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // 斜杠命令补全激活时优先接管导航键
    if (slash && slash.items.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlash((s) =>
          s ? { ...s, selected: (s.selected + 1) % s.items.length } : s,
        );
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlash((s) =>
          s
            ? {
                ...s,
                selected: (s.selected - 1 + s.items.length) % s.items.length,
              }
            : s,
        );
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        applyCommand(slash.items[slash.selected]!);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        slashDismissed.current = `/${slash.query}`;
        setSlash(null);
        return;
      }
    }

    // @ 补全激活时优先接管导航键
    if (mention && mention.results.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMention((m) =>
          m ? { ...m, selected: (m.selected + 1) % m.results.length } : m,
        );
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMention((m) =>
          m
            ? {
                ...m,
                selected:
                  (m.selected - 1 + m.results.length) % m.results.length,
              }
            : m,
        );
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        applyMention(mention.results[mention.selected]!);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        mentionDismissed.current = `@${mention.query}`;
        closeMention();
        return;
      }
    }

    // 中断（处理中双击 Esc，5 秒窗口）
    if (e.key === "Escape") {
      if (isRunning) {
        const now = Date.now();
        if (now - lastEscAt < 5000) {
          setLastEscAt(0);
          setEscHint(false);
          interrupt();
        } else {
          setLastEscAt(now);
          setEscHint(true);
        }
      }
      return;
    }

    // Plan 模式切换
    if (e.key === "Tab" && e.shiftKey) {
      e.preventDefault();
      setPlanMode(!planMode);
      return;
    }

    // 输入历史（光标在首行/末行时）
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      const el = textareaRef.current;
      if (!el) return;
      const beforeCursor = text.slice(0, el.selectionStart);
      const atFirstLine = !beforeCursor.includes("\n");
      const atLastLine = !text.slice(el.selectionEnd).includes("\n");

      const history = useApp.getState().inputHistory;
      if (e.key === "ArrowUp" && atFirstLine && history.length > 0) {
        e.preventDefault();
        const next = historyIndex === null ? history.length - 1 : Math.max(0, historyIndex - 1);
        if (historyIndex === null) setDraft(text);
        setHistoryIndex(next);
        setText(history[next]!);
        return;
      }
      if (e.key === "ArrowDown" && historyIndex !== null && atLastLine) {
        e.preventDefault();
        const next = historyIndex + 1;
        if (next >= history.length) {
          setHistoryIndex(null);
          setText(draft);
        } else {
          setHistoryIndex(next);
          setText(history[next]!);
        }
        return;
      }
    }

    // 粘贴后短窗口内 Enter = 换行（对齐 TUI PasteBurst 语义）
    if (e.key === "Enter" && !e.shiftKey && !e.altKey) {
      if (Date.now() - pastedAt < 350) {
        return; // 让默认行为插入换行
      }
      e.preventDefault();
      submit();
    }
  };

  return (
    <div className="shrink-0 px-4 pb-4 pt-2">
      <div className="mx-auto w-full max-w-3xl">
        {/* 双击 Esc 中断提示（处理中显示） */}
        {escHint && (
          <p className="mb-2 text-center text-[11px] text-muted-foreground/60">
            再按一次 Esc 中断当前任务
          </p>
        )}

        {/* 模式徽标（YOLO 状态在输入框工具栏展示，此处不重复） */}
        {planMode && (
          <div className="mb-2 flex gap-1.5">
            <span className="inline-flex items-center gap-1 rounded-md border border-warning/40 bg-warning/10 px-2 py-0.5 text-xs text-warning">
              <ClipboardList className="size-3" />
              规划模式（只读）· Shift+Tab 退出
            </span>
          </div>
        )}

        <div
          className={cn(
            "relative mx-auto flex w-full flex-col rounded-3xl border border-border bg-background",
            "shadow-sm transition-all duration-200",
            !isRunning &&
              "focus-within:border-primary/30 focus-within:shadow-md focus-within:shadow-primary/5",
          )}
        >
          {/* / 斜杠命令补全下拉（外层圆角裁剪，内层滚动 —— 滚动条不戳出圆角） */}
          {slash && slash.items.length > 0 && (
            <div className="absolute bottom-full left-2 right-2 z-10 mb-2 overflow-hidden rounded-xl border border-border bg-background shadow-lg">
              <div ref={slashListRef} className="max-h-60 overflow-y-auto overscroll-contain py-1">
                {slash.items.map((cmd, i) => (
                  <button
                    key={cmd.name}
                    type="button"
                    data-selected={i === slash.selected}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      applyCommand(cmd);
                    }}
                    className={cn(
                      "flex w-full cursor-pointer items-center gap-2 px-3 py-1.5 text-left text-sm",
                      i === slash.selected
                        ? "bg-accent text-accent-foreground"
                        : "text-foreground/80 hover:bg-muted",
                    )}
                  >
                    <Terminal className="size-3.5 shrink-0 text-muted-foreground/50" />
                    <span className="shrink-0 font-mono text-xs">/{cmd.name}</span>
                    <span className="truncate text-xs text-muted-foreground">
                      {cmd.description}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* @ 文件提及补全下拉（外层圆角裁剪，内层滚动 —— 滚动条不戳出圆角） */}
          {mention && mention.results.length > 0 && (
            <div className="absolute bottom-full left-2 right-2 z-10 mb-2 overflow-hidden rounded-xl border border-border bg-background shadow-lg">
              <div ref={mentionListRef} className="max-h-60 overflow-y-auto overscroll-contain py-1">
                {mention.results.map((rel, i) => (
                  <button
                    key={rel}
                    type="button"
                    data-selected={i === mention.selected}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      applyMention(rel);
                    }}
                    className={cn(
                      "flex w-full cursor-pointer items-center gap-2 px-3 py-1.5 text-left text-sm",
                      i === mention.selected
                        ? "bg-accent text-accent-foreground"
                        : "text-foreground/80 hover:bg-muted",
                    )}
                  >
                    <FileText className="size-3.5 shrink-0 text-muted-foreground/50" />
                    <span className="truncate font-mono text-xs">{rel}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* 外层圆角裁剪，内层滚动 —— 滚动条不戳出圆角 */}
          <div className="overflow-hidden rounded-t-3xl">
            <textarea
              ref={textareaRef}
              value={text}
              rows={2}
              placeholder={
                isRunning
                  ? "处理中…（双击 Esc 中断）"
                  : "给 hailux 发送消息..."
              }
              className="hlx-input-scroll w-full resize-none overflow-y-auto bg-transparent px-4 pb-1 pt-3.5 text-base leading-6 text-foreground outline-none placeholder:text-muted-foreground/40"
              onChange={(e) => {
                setText(e.target.value);
                setHistoryIndex(null);
                const beforeCursor = e.target.value.slice(
                  0,
                  e.target.selectionStart ?? 0,
                );
                // / 命令补全触发（首 token 以 / 开头；与 @ 补全互斥）
                const cmdQuery = slashCommandQuery(beforeCursor);
                if (cmdQuery !== null) {
                  closeMention();
                  if (`/${cmdQuery}` === slashDismissed.current) {
                    // Esc 关闭后片段未变 —— 保持关闭
                  } else {
                    slashDismissed.current = null;
                    const items = commands.filter((c) =>
                      c.name.startsWith(cmdQuery),
                    );
                    setSlash(
                      items.length > 0
                        ? { query: cmdQuery, items, selected: 0 }
                        : null,
                    );
                  }
                  return;
                }
                setSlash(null);
                slashDismissed.current = null;
                // @ 补全触发解析（以光标前文本为准）
                const query = fileMentionQuery(beforeCursor);
                if (query === null) {
                  closeMention();
                } else if (`@${query}` === mentionDismissed.current) {
                  // Esc 关闭后片段未变 —— 保持关闭
                } else {
                  mentionDismissed.current = null;
                  scheduleMentionSearch(query);
                }
              }}
              onPaste={() => setPastedAt(Date.now())}
              onKeyDown={onKeyDown}
            />
          </div>

          <div className="flex min-w-0 items-center justify-between gap-2 px-2.5 pb-2.5 pt-0.5">
            {/* 左侧工具区：Plan / YOLO 开关 / 模型选择 */}
            <div className="flex min-w-0 items-center gap-1">
              <button
                type="button"
                onClick={() => setPlanMode(!planMode)}
                aria-label="切换规划模式"
                title={planMode ? "退出规划模式（Shift+Tab）" : "进入规划模式（只读，Shift+Tab）"}
                className={cn(
                  "flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg px-2 text-xs font-medium transition-colors",
                  planMode
                    ? "bg-warning/10 text-warning"
                    : "text-muted-foreground/40 hover:bg-muted hover:text-muted-foreground",
                )}
              >
                <ClipboardList className="size-3.5" />
                {planMode ? "PLAN" : "CHAT"}
              </button>

              <button
                type="button"
                onClick={() => setYolo(!yolo)}
                aria-label="切换 YOLO 模式"
                title={
                  yolo
                    ? "YOLO：所有权限自动通过，点击恢复确认"
                    : "权限逐项确认，点击进入 YOLO 模式"
                }
                className={cn(
                  "flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg px-2 text-xs font-medium transition-colors",
                  yolo
                    ? "bg-violet-500/15 text-violet-600 dark:bg-violet-400/15 dark:text-violet-400"
                    : "text-muted-foreground/40 hover:bg-muted hover:text-muted-foreground",
                )}
              >
                <Zap className={cn("size-3.5", yolo && "fill-current")} />
                {yolo ? "YOLO" : "ASK"}
              </button>

              <button
                type="button"
                onClick={() => setModelPicker(true)}
                aria-label="切换模型"
                title="切换模型（Ctrl+M）"
                className="flex h-8 min-w-0 shrink cursor-pointer items-center gap-1.5 rounded-lg px-2 text-xs transition-colors hover:bg-muted"
              >
                <Cpu className="size-3.5 shrink-0 text-muted-foreground/50" />
                <span className="truncate text-muted-foreground">
                  {model || "未配置模型"}
                </span>
              </button>
            </div>

            {/* 发送 / 停止 */}
            {isRunning ? (
              <button
                type="button"
                title="停止（双击 Esc）"
                onClick={() => interrupt()}
                className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-destructive text-white shadow-sm transition-all duration-200 hover:bg-destructive/90 active:scale-95"
              >
                {escHint ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <Square className="size-3.5 fill-current" />
                )}
              </button>
            ) : (
              <button
                type="button"
                title="发送（Enter）"
                disabled={!text.trim()}
                onClick={submit}
                className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground shadow-sm transition-all duration-200 hover:bg-primary/90 active:scale-95 disabled:cursor-default disabled:opacity-30"
              >
                <ArrowUp className="size-4" />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
