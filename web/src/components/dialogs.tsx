// 弹窗：权限确认 / 提问 / 模型选择 / 工作目录选择
// （技能与 MCP 管理为内联视图，见 skills-manager.tsx / mcp-manager.tsx）
import { useEffect, useState, type ReactNode } from "react";
import {
  ArrowUp,
  Check,
  ChevronRight,
  Folder,
  FolderOpen,
  HardDrive,
  ShieldAlert,
} from "lucide-react";
import { useApp } from "../store/app-store";
import { cn, shortDir } from "../lib/utils";
import type { FsEntry } from "../runtime/types";

export function Overlay({
  children,
  onClose,
  wide,
}: {
  children: ReactNode;
  /** 不传则 Esc/点击遮罩不可关闭（权限确认等必须作答的弹窗） */
  onClose?: () => void;
  wide?: boolean;
}) {
  useEffect(() => {
    if (!onClose) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className={cn(
          "max-h-[85vh] w-full overflow-y-auto rounded-xl border border-border/60 bg-popover p-5 text-popover-foreground shadow-2xl",
          wide ? "max-w-lg" : "max-w-md",
        )}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

/* 按钮样式（shadcn 风） */
const btnPrimary =
  "cursor-pointer rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30";
const btnOutline =
  "cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground";
const btnDangerOutline =
  "cursor-pointer rounded-lg border border-destructive/50 px-3 py-1.5 text-sm text-destructive transition-colors hover:bg-destructive/10";

/* ── 权限确认 ───────────────────────────────────────────────── */
export function PermissionDialog() {
  const permission = useApp((s) => s.permission);
  const reply = useApp((s) => s.replyPermission);
  if (!permission) return null;

  return (
    <Overlay>
      <div className="mb-3 flex items-center gap-2.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-warning/10 ring-1 ring-warning/30">
          <ShieldAlert className="size-4.5 text-warning" />
        </span>
        <div>
          <h3 className="text-[15px] font-semibold">权限确认</h3>
          {permission.subagent && (
            <p className="text-xs text-muted-foreground/60">
              来自 subagent: {permission.subagent}
            </p>
          )}
        </div>
      </div>

      <p className="mb-3 whitespace-pre-wrap rounded-lg bg-muted/50 p-3 text-sm leading-relaxed">
        {permission.description}
      </p>

      {permission.patterns.length > 0 && (
        <div className="hlx-code mb-4 text-xs">
          {permission.patterns.map((p) => (
            <div key={p}>{p}</div>
          ))}
        </div>
      )}

      <div className="flex justify-end gap-2">
        <button type="button" className={btnDangerOutline} onClick={() => reply(false, false)}>
          拒绝
        </button>
        <button type="button" className={btnOutline} onClick={() => reply(true, false)}>
          允许一次
        </button>
        <button type="button" className={btnPrimary} onClick={() => reply(true, true)}>
          始终允许
        </button>
      </div>
    </Overlay>
  );
}

/* ── 用户提问 ───────────────────────────────────────────────── */
export function AskUserDialog() {
  const askUser = useApp((s) => s.askUser);
  const reply = useApp((s) => s.replyAsk);
  const [answers, setAnswers] = useState<(string | null)[]>([]);

  if (!askUser) return null;
  const questions = askUser.questions;
  const current = answers.length ? answers : questions.map(() => null);

  const allAnswered = current.every(
    (a, i) => a !== null || questions[i]!.options.length === 0,
  );

  const submit = () => {
    const parts = current.map((a, i) => {
      const q = questions[i]!;
      const label = q.options.find((o) => o.label === a)?.label ?? a ?? "";
      return `${q.header}: ${label}`;
    });
    reply(parts.join("\n"));
  };

  return (
    <Overlay>
      {questions.map((q, qi) => (
        <div key={qi} className="mb-4">
          <h4 className="mb-0.5 text-sm font-medium">{q.question}</h4>
          {q.header && (
            <p className="mb-2 text-xs text-muted-foreground/60">{q.header}</p>
          )}
          {q.options.length > 0 ? (
            <div className="space-y-1">
              {q.options.map((o) => (
                <label
                  key={o.label}
                  className={cn(
                    "flex cursor-pointer items-start gap-2.5 rounded-lg border px-3 py-2 text-sm transition-colors",
                    current[qi] === o.label
                      ? "border-primary/30 bg-primary/[0.06]"
                      : "border-border/50 hover:bg-muted/60",
                  )}
                >
                  <input
                    type="radio"
                    name={`q-${qi}`}
                    className="mt-0.5 accent-primary"
                    checked={current[qi] === o.label}
                    onChange={() =>
                      setAnswers((prev) => {
                        const next = prev.length ? [...prev] : questions.map(() => null);
                        next[qi] = o.label;
                        return next;
                      })
                    }
                  />
                  <span>
                    <span className="font-medium">{o.label}</span>
                    {o.description && (
                      <span className="block text-xs text-muted-foreground">
                        {o.description}
                      </span>
                    )}
                  </span>
                </label>
              ))}
            </div>
          ) : (
            <input
              type="text"
              autoFocus
              className="w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30"
              onChange={(e) =>
                setAnswers((prev) => {
                  const next = prev.length ? [...prev] : questions.map(() => null);
                  next[qi] = e.target.value;
                  return next;
                })
              }
            />
          )}
        </div>
      ))}
      <div className="flex justify-end gap-2">
        <button
          type="button"
          className={btnOutline}
          onClick={() => reply("[User Cancelled]")}
        >
          取消
        </button>
        <button
          type="button"
          className={btnPrimary}
          disabled={!allAnswered}
          onClick={submit}
        >
          提交
        </button>
      </div>
    </Overlay>
  );
}

/* ── 模型选择 ───────────────────────────────────────────────── */
export function ModelPicker() {
  const show = useApp((s) => s.showModelPicker);
  const models = useApp((s) => s.models);
  const switchModel = useApp((s) => s.switchModel);
  const setModelPicker = useApp((s) => s.setModelPicker);
  if (!show) return null;

  return (
    <Overlay onClose={() => setModelPicker(false)}>
      <h3 className="mb-3 text-[15px] font-semibold">切换模型</h3>
      <div className="space-y-1">
        {models.map((m) => (
          <button
            type="button"
            key={m.display}
            className={cn(
              "flex w-full cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-left text-sm transition-colors",
              m.active
                ? "border-primary/30 bg-primary/[0.06] text-foreground"
                : "border-border/50 text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
            onClick={() => switchModel(m.display)}
          >
            <span className="flex-1">
              {m.display}
              <span className="block text-xs text-muted-foreground/60">
                {m.provider_name}
              </span>
            </span>
            {m.active && <Check className="size-4 text-foreground" />}
          </button>
        ))}
      </div>
    </Overlay>
  );
}

/* ── 工作目录选择 ───────────────────────────────────────────── */
function DirButton({
  dir,
  active,
  onClick,
}: {
  dir: string;
  active?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={cn(
        "flex w-full cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-left text-sm transition-colors",
        active
          ? "border-primary/30 bg-primary/[0.06] text-foreground"
          : "border-border/50 text-muted-foreground hover:bg-muted/60 hover:text-foreground",
      )}
      title={dir}
      onClick={onClick}
    >
      <Folder className="size-4 shrink-0 text-muted-foreground/70" />
      <span className="min-w-0 flex-1 truncate">{dir || "默认目录"}</span>
      {active && <Check className="size-4 shrink-0 text-foreground" />}
    </button>
  );
}

export function WorkdirPicker() {
  const show = useApp((s) => s.showWorkdirPicker);
  const workdirs = useApp((s) => s.workdirs);
  const workDir = useApp((s) => s.workDir);
  const setWorkDir = useApp((s) => s.setWorkDir);
  const setWorkdirPicker = useApp((s) => s.setWorkdirPicker);
  const [path, setPath] = useState("");
  const [error, setError] = useState("");
  const [fsPath, setFsPath] = useState("");
  const [fsEntries, setFsEntries] = useState<FsEntry[] | null>(null);
  const [fsLoading, setFsLoading] = useState(false);

  useEffect(() => {
    if (!show) {
      setPath("");
      setError("");
      setFsPath("");
      setFsEntries(null);
    }
  }, [show]);

  if (!show) return null;

  const browse = async (p: string) => {
    setFsLoading(true);
    setError("");
    try {
      const url = `/api/fs?dirs_only=true${p ? `&path=${encodeURIComponent(p)}` : ""}`;
      const resp = await fetch(url);
      if (!resp.ok) {
        setError(await resp.text().catch(() => "无法读取目录"));
        return;
      }
      setFsPath(p);
      setFsEntries((await resp.json()) as FsEntry[]);
    } finally {
      setFsLoading(false);
    }
  };

  const validate = async () => {
    setError("");
    const resp = await fetch("/api/workdirs/validate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (resp.ok) {
      const { path: canonical } = (await resp.json()) as { path: string };
      setWorkDir(canonical).catch(() => {});
    } else {
      setError(await resp.text());
    }
  };

  const crumbs = fsPath
    ? fsPath.replace(/\\+/g, "/").split("/").filter(Boolean)
    : [];

  return (
    <Overlay onClose={() => setWorkdirPicker(false)} wide>
      <h3 className="mb-1 text-[15px] font-semibold">选择项目</h3>
      <p className="mb-4 text-xs text-muted-foreground/60">
        切换到所选目录（skills / AGENTS.md / 权限基准随目录切换），不影响已有会话
      </p>

      {workdirs.length > 0 && (
        <>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">最近使用</p>
          <div className="mb-4 space-y-1">
            {workdirs.map((dir) => (
              <DirButton
                key={dir}
                dir={dir}
                active={dir === workDir}
                onClick={() => setWorkDir(dir).catch(() => {})}
              />
            ))}
          </div>
        </>
      )}

      {/* 文件系统浏览 */}
      <p className="mb-1.5 text-xs font-medium text-muted-foreground">浏览文件系统</p>
      {fsEntries === null ? (
        <button
          type="button"
          className="mb-4 flex w-full cursor-pointer items-center gap-2 rounded-lg border border-dashed border-border px-3 py-2.5 text-sm text-muted-foreground transition-colors hover:border-primary/30 hover:text-foreground"
          onClick={() => browse("")}
        >
          <HardDrive className="size-4" />
          从根目录开始浏览
        </button>
      ) : (
        <div className="mb-4">
          {/* 面包屑 */}
          <div className="mb-1.5 flex flex-wrap items-center gap-0.5 rounded-lg bg-muted/50 px-2 py-1.5 text-xs">
            <button
              type="button"
              className="cursor-pointer rounded px-1 py-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              onClick={() => {
                setFsPath("");
                setFsEntries(null);
              }}
            >
              根
            </button>
            {crumbs.map((c, i) => (
              <span key={i} className="flex items-center gap-0.5">
                <ChevronRight className="size-3 text-muted-foreground/40" />
                <button
                  type="button"
                  className="max-w-40 cursor-pointer truncate rounded px-1 py-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  title={crumbs.slice(0, i + 1).join("/")}
                  onClick={() => browse(crumbs.slice(0, i + 1).join("/"))}
                >
                  {c}
                </button>
              </span>
            ))}
          </div>
          {fsLoading ? (
            <p className="px-2 py-2 text-xs text-muted-foreground/60">读取中…</p>
          ) : (
            <div className="max-h-52 space-y-0.5 overflow-y-auto rounded-lg border border-border/50 p-1">
              {fsEntries.length === 0 && (
                <p className="px-2 py-2 text-xs text-muted-foreground/60">无子目录</p>
              )}
              {fsEntries.map((e) => (
                <button
                  type="button"
                  key={e.path}
                  className="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
                  title={e.path}
                  onClick={() => browse(e.path)}
                >
                  <Folder className="size-3.5 text-muted-foreground/60" />
                  <span className="truncate">{e.name}</span>
                </button>
              ))}
            </div>
          )}
          {fsPath && (
            <button
              type="button"
              className={cn(btnPrimary, "mt-2 flex w-full items-center justify-center gap-1.5 py-2")}
              onClick={() => setWorkDir(fsPath).catch(() => {})}
            >
              <FolderOpen className="size-4" />
              进入 {shortDir(fsPath)}
            </button>
          )}
        </div>
      )}

      {/* 手动输入 */}
      <p className="mb-1.5 text-xs font-medium text-muted-foreground">输入路径</p>
      <div className="flex gap-2">
        <input
          value={path}
          placeholder="D:\project\my-app"
          className="flex-1 rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30"
          onChange={(e) => setPath(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && path.trim() && validate()}
        />
        <button type="button" className={cn(btnPrimary, "px-4")} onClick={validate}>
          打开
        </button>
      </div>
      {error && <p className="mt-2 text-xs text-destructive">{error}</p>}

      {/* 返回上级 */}
      {fsEntries !== null && fsPath && (
        <button
          type="button"
          className="mt-2 flex cursor-pointer items-center gap-1 text-xs text-muted-foreground/60 transition-colors hover:text-foreground"
          onClick={() => browse(crumbs.slice(0, -1).join("/"))}
        >
          <ArrowUp className="size-3" />
          返回上级
        </button>
      )}
    </Overlay>
  );
}
