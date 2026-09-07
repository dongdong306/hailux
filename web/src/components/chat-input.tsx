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
  X,
  Zap,
} from "lucide-react";
import { anyDialogOpen, useApp } from "../store/app-store";
import type { ChatAttachment, CommandInfo } from "../runtime/types";
import { cn, fmtTokens } from "../lib/utils";

/** 单张待发送附件大小上限（10 MB，对齐后端 ATTACHMENT_MAX_BYTES） */
const ATTACHMENT_MAX_BYTES = 10 * 1024 * 1024;
/** 单条消息附件数量上限（对齐后端） */
const ATTACHMENT_MAX_COUNT = 6;
/** 受支持的图片类型（对齐后端 is_supported_image_mime，SVG/BMP 等后端会 400） */
const SUPPORTED_IMAGE_MIMES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
]);

/** 从文件读取 data URL 附件（仅图片，超出大小上限时返回带错误的结果） */
function fileToAttachment(file: File): Promise<ChatAttachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () =>
      resolve({ mime: file.type, data_url: reader.result as string });
    reader.onerror = () => reject(new Error(`读取失败: ${file.name}`));
    reader.readAsDataURL(file);
  });
}

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

/** 上下文占用环形指示（16px）：底环 muted + 进度环 accent，中心不留字（数值在旁边） */
function ContextRing({ pct }: { pct: number | null }) {
  const r = 6;
  const c = 2 * Math.PI * r;
  const ratio = pct === null ? 0 : Math.min(pct, 1);
  const color =
    pct === null
      ? "text-muted-foreground/40"
      : ratio >= 0.9
        ? "text-destructive"
        : ratio >= 0.8
          ? "text-warning"
          : "text-meter-cache";
  return (
    <svg
      viewBox="0 0 16 16"
      className={cn("size-4 shrink-0 -rotate-90", color)}
      aria-hidden="true"
    >
      <circle
        cx="8"
        cy="8"
        r={r}
        fill="none"
        strokeWidth="2.5"
        className="stroke-muted-foreground/20"
      />
      <circle
        cx="8"
        cy="8"
        r={r}
        fill="none"
        strokeWidth="2.5"
        strokeLinecap="round"
        stroke="currentColor"
        strokeDasharray={c}
        strokeDashoffset={c * (1 - ratio)}
        style={{ transition: "stroke-dashoffset 0.6s ease" }}
      />
    </svg>
  );
}

export function ChatInput() {
  const [text, setText] = useState("");
  const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [lastEscAt, setLastEscAt] = useState(0);
  const [pastedAt, setPastedAt] = useState(0);
  const [mention, setMention] = useState<MentionState | null>(null);
  const [slash, setSlash] = useState<SlashState | null>(null);
  const [pendingImages, setPendingImages] = useState<ChatAttachment[]>([]);
  /** 附件被忽略的提示（超限/类型不支持），提交或再次粘贴时清除 */
  const [imageNotice, setImageNotice] = useState<string | null>(null);
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
  // 上下文占用（最后请求的输入+输出 ≈ 当前上下文大小）
  const contextPromptTokens = useApp((s) => s.contextPromptTokens);
  const contextCompletionTokens = useApp((s) => s.contextCompletionTokens);
  const contextWindow = useApp((s) => s.contextWindow);
  const contextUsed = contextPromptTokens + contextCompletionTokens;
  const contextPct =
    contextWindow > 0 ? Math.min(contextUsed / contextWindow, 1) : null;

  useEffect(() => {
    if (!escHint) return;
    const t = setTimeout(() => setEscHint(false), 5000);
    return () => clearTimeout(t);
  }, [escHint, setEscHint]);

  const sessionId = useApp((s) => s.sessionId);
  const workDir = useApp((s) => s.workDir);
  useEffect(() => {
    // 切换会话/项目时清空待发送附件与提示，避免图片误发进新会话
    setPendingImages([]);
    setImageNotice(null);
  }, [sessionId, workDir]);

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
    if ((!text.trim() && pendingImages.length === 0) || isRunning) return;
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

    const attachments = pendingImages;
    setText("");
    setPendingImages([]);
    setImageNotice(null);
    setHistoryIndex(null);
    setDraft("");
    sendMessage(value, attachments.length > 0 ? attachments : undefined);
  };

  /** 追加粘贴/选择的图片附件（类型/数量/大小校验与后端一致） */
  const addImageFiles = async (files: File[]) => {
    const images = files.filter((f) => SUPPORTED_IMAGE_MIMES.has(f.type));
    const rejected = files.length - images.length;
    if (images.length === 0) {
      if (rejected > 0)
        setImageNotice(`不支持的图片类型（仅支持 PNG/JPEG/GIF/WebP）`);
      return;
    }
    let oversize = 0;
    for (const file of images) {
      if (file.size > ATTACHMENT_MAX_BYTES) {
        oversize++;
        continue;
      }
      if (pendingImages.length >= ATTACHMENT_MAX_COUNT) break;
      try {
        const att = await fileToAttachment(file);
        setPendingImages((prev) =>
          prev.length >= ATTACHMENT_MAX_COUNT ? prev : [...prev, att],
        );
      } catch {
        // 读取失败：跳过
      }
    }
    if (pendingImages.length >= ATTACHMENT_MAX_COUNT) {
      setImageNotice(`最多附加 ${ATTACHMENT_MAX_COUNT} 张图片`);
    } else if (oversize > 0) {
      setImageNotice(`${oversize} 张图片超过 10MB 已忽略`);
    } else if (rejected > 0) {
      setImageNotice(`${rejected} 个文件类型不支持（仅支持 PNG/JPEG/GIF/WebP）`);
    }
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
      // 全局弹窗打开时 Esc 只关弹窗，不进入中断计时
      if (anyDialogOpen()) return;
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
              onPaste={(e) => {
                setPastedAt(Date.now());
                // 粘贴图片：拦截并转为附件 chips。仅当剪贴板同时没有文本时
                // 才整体拦截——图文混合时文本走默认插入、图片另行追加
                const files = Array.from(e.clipboardData?.files ?? []);
                const hasImage = files.some((f) =>
                  SUPPORTED_IMAGE_MIMES.has(f.type),
                );
                const hasText = (e.clipboardData?.getData("text/plain") ?? "").length > 0;
                if (hasImage && !hasText) {
                  e.preventDefault();
                  void addImageFiles(files);
                }
              }}
              onKeyDown={onKeyDown}
            />
          </div>

          {/* 图片附件 chips：缩略图 + 移除按钮 */}
          {imageNotice && (
            <div className="px-4 pb-1 pt-1 text-xs text-muted-foreground">
              {imageNotice}
            </div>
          )}
          {pendingImages.length > 0 && (
            <div className="flex flex-wrap gap-2 px-4 pb-2 pt-1">
              {pendingImages.map((att, i) => (
                <div
                  key={`${att.data_url.slice(-16)}-${i}`}
                  className="group relative size-14 overflow-hidden rounded-lg border border-border"
                >
                  <img
                    src={att.data_url}
                    alt={att.mime}
                    loading="lazy"
                    decoding="async"
                    onError={(ev) => {
                      ev.currentTarget.style.opacity = "0.3";
                    }}
                    className="size-full object-cover"
                  />
                  <button
                    type="button"
                    aria-label="移除图片"
                    onClick={() =>
                      setPendingImages((prev) => prev.filter((_, j) => j !== i))
                    }
                    className="absolute right-0.5 top-0.5 hidden size-4 cursor-pointer items-center justify-center rounded-full bg-background/80 text-foreground group-hover:flex"
                  >
                    <X className="size-3" />
                  </button>
                </div>
              ))}
            </div>
          )}

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

            {/* 右侧：上下文环 + 发送/停止（紧挨） */}
            <div className="flex shrink-0 items-center gap-1.5">
              {/* 上下文占用环形指示（常驻）：最后请求的输入+输出（≈当前上下文大小）/ 窗口。
                  处理中数值是上一轮的，仍保持展示不闪烁；数值收进 tooltip */}
              <span
                className={cn(
                  "flex h-8 cursor-default items-center rounded-lg px-1.5",
                  isRunning && "opacity-60",
                )}
                title={`上下文占用 ${fmtTokens(contextUsed)}${contextWindow > 0 ? ` / ${fmtTokens(contextWindow)} · ${Math.round((contextPct ?? 0) * 100)}%` : ""}`}
              >
                <ContextRing pct={contextPct} />
              </span>

              {/* 发送 / 停止 */}
              {isRunning ? (
                <button
                  type="button"
                  title="停止（双击 Esc）"
                  onClick={() => interrupt()}
                  className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground shadow-sm transition-all duration-200 hover:bg-primary/90 active:scale-95"
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
                  disabled={!text.trim() && pendingImages.length === 0}
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
    </div>
  );
}
