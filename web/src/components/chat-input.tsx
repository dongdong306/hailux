import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ArrowUp,
  ChevronDown,
  ClipboardList,
  ListPlus,
  Loader2,
  MessageCircle,
  Pencil,
  Square,
  Terminal,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import { anyDialogOpen, useApp } from "../store/app-store";
import type { ChatAttachment, ChatMention, CommandInfo } from "../runtime/types";
import type { DeferredSettingAction } from "../store/types";
import { cn } from "../lib/utils";
import { MentionIcon } from "./mention-icon";
import { ModelEffortMenu } from "./model-effort-menu";

function utf16ToCharIndex(text: string, u16Idx: number): number {
  return [...text.slice(0, u16Idx)].length;
}

const ATTACHMENT_MAX_BYTES = 10 * 1024 * 1024;
const ATTACHMENT_MAX_COUNT = 6;
/** Supported image types (mirrors backend is_supported_image_mime; SVG/BMP etc. 400) */
const SUPPORTED_IMAGE_MIMES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
]);

function fileToAttachment(file: File): Promise<ChatAttachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () =>
      resolve({ mime: file.type, data_url: reader.result as string });
    reader.onerror = () => reject(new Error(`读取失败: ${file.name}`));
    reader.readAsDataURL(file);
  });
}

function fileMentionQuery(beforeCursor: string): string | null {
  const atIdx = beforeCursor.lastIndexOf("@");
  if (atIdx < 0) return null;
  const beforeAt = beforeCursor.slice(0, atIdx);
  if (beforeAt && !/\s/.test(beforeAt[beforeAt.length - 1]!)) return null;
  const query = beforeCursor.slice(atIdx + 1);
  if (/\s/.test(query)) return null;
  return query;
}

function slashCommandQuery(beforeCursor: string): string | null {
  if (!beforeCursor.startsWith("/") || /\s/.test(beforeCursor)) return null;
  return beforeCursor.slice(1);
}

function parseSettingAction(
  name: string,
  args: string,
): DeferredSettingAction | null {
  if (name === "plan" || name === "yolo" || name === "compact") {
    return { kind: name };
  }
  if (name === "effort") {
    const v = args.trim().toLowerCase();
    return v === "low" || v === "high" || v === "max"
      ? { kind: "effort", value: v }
      : null;
  }
  return null;
}

interface MentionState {
  query: string;
  results: string[];
  selected: number;
  loading: boolean;
}

interface MentionRange {
  start: number;
  end: number;
}

function computeMentionRanges(value: string, tokens: Set<string>): MentionRange[] {
  const ranges: MentionRange[] = [];
  for (const token of tokens) {
    let idx = value.indexOf(token);
    while (idx !== -1) {
      ranges.push({ start: idx, end: idx + token.length });
      idx = value.indexOf(token, idx + token.length);
    }
  }
  ranges.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: MentionRange[] = [];
  let lastEnd = -1;
  for (const r of ranges) {
    if (r.start < lastEnd) continue;
    out.push(r);
    lastEnd = r.end;
  }
  return out;
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
  const [pendingImages, setPendingImages] = useState<ChatAttachment[]>([]);
  const [imageNotice, setImageNotice] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const mentionListRef = useRef<HTMLDivElement>(null);
  const slashListRef = useRef<HTMLDivElement>(null);
  const mentionTimer = useRef<number | null>(null);
  const mentionAbort = useRef<AbortController | null>(null);
  const mentionDismissed = useRef<string | null>(null);
  const slashDismissed = useRef<string | null>(null);
  const pendingMentions = useRef(new Map<string, string>());
  const mentionTokens = useRef(new Set<string>());
  const [mentionRanges, setMentionRanges] = useState<MentionRange[]>([]);
  const [mentionScrollTop, setMentionScrollTop] = useState(0);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const [selRange, setSelRange] = useState<{ start: number; end: number } | null>(
    null,
  );
  const mentionMirrorRef = useRef<HTMLDivElement>(null);

  const setTextWithRanges = (value: string) => {
    setText(value);
    setMentionRanges(computeMentionRanges(value, mentionTokens.current));
    setHoverIdx(null);
    setSelRange(null);
  };

  const handleMentionHover = (e: React.MouseEvent) => {
    const mirror = mentionMirrorRef.current;
    if (!mirror) return;
    let next: number | null = null;
    mirror.querySelectorAll<HTMLElement>("[data-mention-idx]").forEach((s) => {
      if (next !== null) return;
      const r = s.getBoundingClientRect();
      if (
        e.clientX >= r.left &&
        e.clientX <= r.right &&
        e.clientY >= r.top &&
        e.clientY <= r.bottom
      ) {
        next = Number(s.dataset.mentionIdx);
      }
    });
    setHoverIdx((prev) => (prev === next ? prev : next));
  };

  const applyTextEdit = (value: string, cursor: number) => {
    setTextWithRanges(value);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (el) {
        el.selectionStart = el.selectionEnd = cursor;
        setSelRange(null);
      }
    });
  };

  const mentionSegments = useMemo(() => {
    const segs: Array<{ key: string; text: string; mention: boolean }> = [];
    let pos = 0;
    for (const r of mentionRanges) {
      if (r.start > pos)
        segs.push({ key: `p${pos}`, text: text.slice(pos, r.start), mention: false });
      const body = text.slice(r.start, r.end);
      const chip = body.endsWith(" ") ? body.slice(0, -1) : body;
      segs.push({ key: `m${r.start}`, text: chip, mention: true });
      if (body.length > chip.length)
        segs.push({ key: `t${r.start}`, text: " ", mention: false });
      pos = r.end;
    }
    if (pos < text.length)
      segs.push({ key: `p${pos}`, text: text.slice(pos), mention: false });
    return segs;
  }, [text, mentionRanges]);

  const isRunning = useApp((s) => s.isRunning);
  const planMode = useApp((s) => s.planMode);
  const sendMessage = useApp((s) => s.sendMessage);
  const interrupt = useApp((s) => s.interrupt);
  const setPlanMode = useApp((s) => s.setPlanMode);
  const escHint = useApp((s) => s.escHint);
  const setEscHint = useApp((s) => s.setEscHint);
  const yolo = useApp((s) => s.yolo);
  const setYolo = useApp((s) => s.setYolo);
  const commands = useApp((s) => s.commands);
  const setView = useApp((s) => s.setView);
  const setModelMenu = useApp((s) => s.setModelMenu);
  const deferSetting = useApp((s) => s.deferSetting);
  const applySettingAction = useApp((s) => s.applySettingAction);
  const sessionId = useApp((s) => s.sessionId);
  const queue = useApp((s) => (s.sessionId ? s.queuedMessages[s.sessionId] : undefined)) ?? [];
  const [queueExpanded, setQueueExpanded] = useState(true);
  const enqueueMessage = useApp((s) => s.enqueueMessage);
  const removeQueuedMessage = useApp((s) => s.removeQueuedMessage);
  const interjectQueued = useApp((s) => s.interjectQueued);
  const interjectAllQueued = useApp((s) => s.interjectAllQueued);
  const hasContent = text.trim().length > 0 || pendingImages.length > 0;

  useEffect(() => {
    if (!escHint) return;
    const t = setTimeout(() => setEscHint(false), 5000);
    return () => clearTimeout(t);
  }, [escHint, setEscHint]);

  const workDir = useApp((s) => s.workDir);
  useEffect(() => {
    setPendingImages([]);
    setImageNotice(null);
  }, [sessionId, workDir]);

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
        // Aborted/stale mention search — the next keystroke reschedules
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
    const workDirRaw = useApp.getState().workDir.replace(/[\\/]+$/, "");
    const sep =
      workDirRaw.includes("\\") || /^[a-zA-Z]:[\\/]/.test(workDirRaw)
        ? "\\"
        : "/";
    let abs = workDirRaw + sep + rel.replace(/\//g, sep);
    if (sep === "\\") abs = abs.replace(/\//g, "\\");
    pendingMentions.current.set(display, abs);
    const element = `${display} `;
    mentionTokens.current.add(element);
    setTextWithRanges(text.slice(0, atIdx) + element + text.slice(cursor));
    closeMention();
    requestAnimationFrame(() => {
      el.focus();
      const pos = atIdx + element.length;
      el.selectionStart = el.selectionEnd = pos;
    });
  };

  const applyCommand = (cmd: CommandInfo) => {
    const el = textareaRef.current;
    mentionTokens.current.clear();
    setTextWithRanges(`/${cmd.name} `);
    setSlash(null);
    slashDismissed.current = null;
    requestAnimationFrame(() => {
      el?.focus();
      const pos = cmd.name.length + 2;
      if (el) el.selectionStart = el.selectionEnd = pos;
    });
  };

  const expandMentions = (value: string) => {
    let out = value;
    for (const [display, abs] of pendingMentions.current) {
      out = out.split(display).join(`@${abs}`);
    }
    const claimed: Array<[number, number]> = [];
    const mentions: ChatMention[] = [];
    const ordered = [...new Set([...pendingMentions.current.values()])].sort(
      (a, b) => b.length - a.length,
    );
    for (const abs of ordered) {
      const token = `@${abs}`;
      let from = 0;
      for (;;) {
        const idx = out.indexOf(token, from);
        if (idx === -1) break;
        from = idx + token.length;
        if (claimed.some(([s, e]) => idx < e && from > s)) continue;
        claimed.push([idx, from]);
        mentions.push({
          start: utf16ToCharIndex(out, idx),
          end: utf16ToCharIndex(out, from),
          kind: "file",
        });
      }
    }
    mentions.sort((a, b) => a.start - b.start);
    pendingMentions.current.clear();
    return { out, mentions };
  };

  const enqueueCurrent = () => {
    const { out: value, mentions } = expandMentions(text.trim());
    mentionTokens.current.clear();
    const attachments = pendingImages;
    setTextWithRanges("");
    setPendingImages([]);
    setImageNotice(null);
    setHistoryIndex(null);
    setDraft("");
    enqueueMessage(value, attachments.length > 0 ? attachments : undefined, mentions);
  };

  const editQueued = (item: (typeof queue)[number]) => {
    if (!sessionId) return;
    if (item.inFlight) return; // delivery pending; editing would duplicate it
    removeQueuedMessage(sessionId, item.id);
    mentionTokens.current.clear();
    setPendingImages(item.images ?? []);
    setTextWithRanges(item.text);
    requestAnimationFrame(() => textareaRef.current?.focus());
  };

  const runViewCommand = (name: string) => {
    if (name === "skills") setView("skills");
    else if (name === "mcp") setView("mcp");
    else if (name === "stats") setView("stats");
    else if (name === "models") setModelMenu(true);
  };

  const pushNotice = (t: string) => {
    useApp.getState().pushTurnItem({ kind: "notice", text: t });
  };

  const submit = () => {
    if (!hasContent) return;
    const raw = text.trim();
    const firstWord = raw.split(/\s+/, 1)[0] ?? "";
    const cmd = firstWord.startsWith("/")
      ? useApp.getState().commands.find((c) => c.name === firstWord.slice(1))
      : undefined;
    const args = raw.slice(firstWord.length).trim();

    const clearInput = () => {
      setTextWithRanges("");
      setPendingImages([]);
      setImageNotice(null);
      setHistoryIndex(null);
      setDraft("");
    };

    if (cmd?.kind === "view") {
      clearInput();
      runViewCommand(cmd.name);
      return;
    }

    if (cmd?.kind === "setting") {
      if (cmd.name === "compact" && isRunning) {
        if (sessionId) {
          clearInput();
          deferSetting(sessionId, { kind: "compact" });
          pushNotice("已暂存 /compact：将在当前任务完成后执行");
        } else {
          pushNotice("当前没有活动会话，无法暂存 /compact");
        }
        return;
      }
      if (cmd.name === "effort" && !args.trim()) {
        clearInput();
        setModelMenu(true);
        return;
      }
      const action = parseSettingAction(cmd.name, args);
      clearInput();
      if (!action) {
        pushNotice(
          cmd.name === "effort"
            ? "无效的思考强度，可选值 low / high / max"
            : `无效参数：/${cmd.name} ${args}`,
        );
        return;
      }
      applySettingAction(action);
      return;
    }

    if (isRunning) {
      enqueueCurrent();
      return;
    }

    const { out: value, mentions } = expandMentions(raw);
    mentionTokens.current.clear();

    const attachments = pendingImages;
    clearInput();
    sendMessage(value, attachments.length > 0 ? attachments : undefined, mentions);
  };

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
        // Read failure: the user already sees the remaining count/notice below
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

    const areaEl = textareaRef.current;
    if (areaEl && mentionRanges.length > 0) {
      const ss = areaEl.selectionStart ?? 0;
      const se = areaEl.selectionEnd ?? 0;
      if (e.key === "Backspace" || e.key === "Delete") {
        if (ss < se) {
          let start = ss;
          let end = se;
          for (const r of mentionRanges) {
            if (r.start < end && r.end > start) {
              start = Math.min(start, r.start);
              end = Math.max(end, r.end);
            }
          }
          if (start !== ss || end !== se) {
            e.preventDefault();
            applyTextEdit(text.slice(0, start) + text.slice(end), start);
            return;
          }
        } else {
          const r =
            e.key === "Backspace"
              ? mentionRanges.find((x) => ss > x.start && ss <= x.end)
              : mentionRanges.find((x) => ss >= x.start && ss < x.end);
          if (r) {
            e.preventDefault();
            applyTextEdit(text.slice(0, r.start) + text.slice(r.end), r.start);
            return;
          }
        }
      } else if (e.key === "ArrowLeft" && ss === se) {
        const r = mentionRanges.find((x) => ss > x.start && ss <= x.end);
        if (r) {
          e.preventDefault();
          areaEl.selectionStart = areaEl.selectionEnd = r.start;
          return;
        }
      } else if (e.key === "ArrowRight" && ss === se) {
        const r = mentionRanges.find((x) => ss >= x.start && ss < x.end);
        if (r) {
          e.preventDefault();
          areaEl.selectionStart = areaEl.selectionEnd = r.end;
          return;
        }
      }
    }

    if (e.key === "Escape") {
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

    if (e.key === "Tab" && e.shiftKey) {
      e.preventDefault();
      setPlanMode(!planMode);
      return;
    }

    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      // Same paste-protection window as plain Enter: a paste ending in an
      // image+text burst must not instantly interject the whole queue
      if (Date.now() - pastedAt < 350) {
        return;
      }
      e.preventDefault();
      if (sessionId && queue.length > 0) void interjectAllQueued(sessionId);
      return;
    }

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
        mentionTokens.current.clear();
        setTextWithRanges(history[next]!);
        return;
      }
      if (e.key === "ArrowDown" && historyIndex !== null && atLastLine) {
        e.preventDefault();
        const next = historyIndex + 1;
        if (next >= history.length) {
          setHistoryIndex(null);
          mentionTokens.current.clear();
          setTextWithRanges(draft);
        } else {
          setHistoryIndex(next);
          mentionTokens.current.clear();
          setTextWithRanges(history[next]!);
        }
        return;
      }
    }

    // Enter within the short window after a paste = newline (TUI PasteBurst
    // semantics)
    if (e.key === "Enter" && !e.shiftKey && !e.altKey) {
      if (Date.now() - pastedAt < 350) {
        return; // let the default behavior insert the newline
      }
      e.preventDefault();
      submit();
    }
  };

  return (
    <div className="shrink-0 px-4 pb-4 pt-2">
      <div className="mx-auto w-full max-w-3xl">
        {escHint && (
          <p className="mb-2 text-center text-[11px] text-muted-foreground/60">
            再按一次 Esc 中断当前任务
          </p>
        )}

        {queue.length > 0 && (
          <div className="mx-auto mb-2 w-[94%] rounded-2xl border border-border bg-background shadow-sm">
            {queue.length > 1 && (
              <button
                type="button"
                onClick={() => setQueueExpanded((v) => !v)}
                aria-label={queueExpanded ? "收起排队消息" : "展开排队消息"}
                className="flex w-full cursor-pointer items-center gap-2 px-3 py-2 text-left text-sm"
              >
                <MessageCircle className="size-4 shrink-0 text-muted-foreground" />
                <span className="font-medium text-foreground">
                  {queue.length} 条排队消息
                </span>
                <ChevronDown
                  className={cn(
                    "ml-auto size-4 shrink-0 text-muted-foreground/60 transition-transform",
                    queueExpanded && "rotate-180",
                  )}
                />
              </button>
            )}
            {(queueExpanded || queue.length === 1) && (
              <div
                className={cn(
                  "max-h-48 overflow-y-auto overscroll-contain py-1",
                  queue.length > 1 && "border-t border-border/60",
                )}
              >
                {queue.map((item) => (
                  <div
                    key={item.id}
                    className="flex items-center gap-2 px-3 py-1.5"
                  >
                    <MessageCircle className="size-4 shrink-0 text-muted-foreground/60" />
                    <span
                      className={cn(
                        "min-w-0 flex-1 truncate text-sm",
                        item.inFlight
                          ? "text-muted-foreground/50"
                          : "text-foreground/90",
                      )}
                    >
                      {item.text}
                    </span>
                    {item.inFlight ? (
                      <span className="shrink-0 text-xs text-muted-foreground/50">
                        发送中…
                      </span>
                    ) : (
                      <div className="flex shrink-0 items-center gap-0.5 text-muted-foreground/60">
                        <button
                          type="button"
                          title="编辑（放回输入框）"
                          onClick={() => editQueued(item)}
                          className="cursor-pointer rounded-md p-1 hover:bg-muted hover:text-foreground"
                        >
                          <Pencil className="size-3.5" />
                        </button>
                        <button
                          type="button"
                          title="删除"
                          onClick={() =>
                            sessionId && removeQueuedMessage(sessionId, item.id)
                          }
                          className="cursor-pointer rounded-md p-1 hover:bg-muted hover:text-foreground"
                        >
                          <Trash2 className="size-3.5" />
                        </button>
                        <button
                          type="button"
                          title="插话发送"
                          onClick={() =>
                            sessionId && void interjectQueued(sessionId, item)
                          }
                          className="cursor-pointer rounded-md p-1 hover:bg-muted hover:text-foreground"
                        >
                          <ArrowUp className="size-3.5" />
                        </button>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
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
                    <MentionIcon rel={rel} />
                    <span className="truncate font-mono text-xs">{rel}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          <div
            className="relative overflow-hidden rounded-t-3xl"
            onMouseMove={handleMentionHover}
            onMouseLeave={() => setHoverIdx(null)}
          >
            <textarea
              ref={textareaRef}
              value={text}
              rows={2}
              placeholder={
                queue.length > 0
                  ? "Cmd/Ctrl+Enter 插话发送全部排队消息"
                  : isRunning
                    ? "处理中…（双击 Esc 中断；输入内容 Enter 排队发送）"
                    : "给 hailux 发送消息..."
              }
              className={cn(
                "relative hlx-input-scroll w-full resize-none overflow-y-auto bg-transparent px-4 pb-1 pt-3.5 text-base leading-6 text-foreground outline-none placeholder:text-muted-foreground/40 [scrollbar-gutter:stable]",
                "selection:bg-blue-500/15",
                hoverIdx !== null && "cursor-pointer",
              )}
              onSelect={() => {
                const el = textareaRef.current;
                if (!el) return;
                const ss = el.selectionStart ?? 0;
                const se = el.selectionEnd ?? 0;
                setSelRange(ss !== se ? { start: ss, end: se } : null);
                if (ss !== se) return;
                const pos = ss;
                const r = mentionRanges.find(
                  (x) => pos > x.start && pos < x.end,
                );
                if (r) {
                  const target = pos - r.start <= r.end - pos ? r.start : r.end;
                  el.selectionStart = el.selectionEnd = target;
                }
              }}
              onScroll={(e) => setMentionScrollTop(e.currentTarget.scrollTop)}
              onChange={(e) => {
                setTextWithRanges(e.target.value);
                setHistoryIndex(null);
                const beforeCursor = e.target.value.slice(
                  0,
                  e.target.selectionStart ?? 0,
                );
                const cmdQuery = slashCommandQuery(beforeCursor);
                if (cmdQuery !== null) {
                  closeMention();
                  if (`/${cmdQuery}` === slashDismissed.current) {
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
                const query = fileMentionQuery(beforeCursor);
                if (query === null) {
                  closeMention();
                } else if (`@${query}` === mentionDismissed.current) {
                                  } else {
                  mentionDismissed.current = null;
                  scheduleMentionSearch(query);
                }
              }}
              onPaste={(e) => {
                setPastedAt(Date.now());
                // Image paste: intercept and convert to attachment chips, but
                // only when the clipboard holds no text at all — with mixed
                // content, text takes the default insertion and images are
                // appended separately
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
            {/* Chip mirror layer: padding and scrollbar-gutter must match the textarea so line wraps stay in sync */}
            <div
              aria-hidden
              className="pointer-events-none absolute inset-0 z-10 overflow-hidden px-4 pb-1 pt-3.5 text-base leading-6 [scrollbar-gutter:stable]"
            >
              <div
                ref={mentionMirrorRef}
                className="whitespace-pre-wrap break-words text-transparent"
                style={{ transform: `translateY(-${mentionScrollTop}px)` }}
              >
                {(() => {
                  let chipIdx = -1;
                  return mentionSegments.map((seg, i) => {
                    if (!seg.mention) {
                      return <span key={seg.key}>{seg.text}</span>;
                    }
                    chipIdx++;
                    const range = mentionRanges[chipIdx];
                    const selected =
                      !!range &&
                      !!selRange &&
                      selRange.start < range.end &&
                      selRange.end > range.start;
                    // Zero padding on the chip itself: pixel-aligns with the
                    // native glyphs; the icon positions at the `@` glyph
                    return (
                      <span
                        key={seg.key}
                        data-mention-idx={i}
                        className={cn(
                          "relative rounded-md text-blue-600 dark:text-blue-400",
                          selected
                            ? "bg-[color-mix(in_srgb,var(--color-blue-500)_15%,var(--background))]"
                            : "bg-background",
                        )}
                      >
                        {hoverIdx === i && (
                          <span
                            aria-hidden
                            className="absolute -inset-x-1 -inset-y-0.5 rounded-md bg-blue-500/10 dark:bg-blue-400/10"
                          />
                        )}
                        <span aria-hidden className="relative rounded-md">
                          <span className="text-transparent">{seg.text[0]}</span>
                          {seg.text.slice(1)}
                        </span>
                        <MentionIcon
                          rel={seg.text.slice(1)}
                          mono
                          className="absolute -left-0.5 top-[3px] size-3.5"
                        />
                      </span>
                    );
                  });
                })()}
              </div>
            </div>
          </div>

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
            </div>

              <div className="flex shrink-0 items-center gap-1.5">
                <ModelEffortMenu />

                {isRunning ? (
                  hasContent ? (
                    <button
                      type="button"
                      title="排队发送（Enter）"
                      onClick={submit}
                      className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground shadow-sm transition-all duration-200 hover:bg-primary/90 active:scale-95"
                    >
                      <ListPlus className="size-4" />
                    </button>
                  ) : (
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
                  )
                ) : (
                  <button
                    type="button"
                    title="发送（Enter）"
                    disabled={!hasContent}
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
