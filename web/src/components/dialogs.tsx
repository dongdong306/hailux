// Dialogs: permission confirmation / ask user / workdir picker
// (Skills and MCP management are inline views, see skills-manager.tsx /
//   mcp-manager.tsx; model provider management is in settings-view.tsx;
//   model + reasoning-effort switching is in model-effort-menu.tsx)
import { useEffect, useState, type ReactNode } from "react";
import { Check, Folder, ShieldAlert } from "lucide-react";
import { useApp } from "../store/app-store";
import { cn } from "../lib/utils";
import type { QuestionInfo } from "../runtime/types";

export function Overlay({
  children,
  onClose,
  wide,
  closeOnMask = true,
}: {
  children: ReactNode;
  /** If omitted, the dialog cannot be closed via Esc or mask click
   *  (dialogs that require an answer, e.g. permission confirmation) */
  onClose?: () => void;
  wide?: boolean;
  /** When false, Esc still closes but clicking the mask does not
   *  (prevents accidentally discarding entered content) */
  closeOnMask?: boolean;
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
      onClick={closeOnMask ? onClose : undefined}
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

/* Button styles (shadcn style) */
const btnPrimary =
  "cursor-pointer rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30";
const btnOutline =
  "cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground";
const btnDangerOutline =
  "cursor-pointer rounded-lg border border-destructive/50 px-3 py-1.5 text-sm text-destructive transition-colors hover:bg-destructive/10";

/* ── Permission confirmation ───────────────────────────────────────────────── */
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

      <p className="mb-3 max-h-[50vh] overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-muted/50 p-3 text-sm leading-relaxed">
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

/* ── Ask user ───────────────────────────────────────────────── */

/** Submit format aligned with the TUI AskUserState::submit_and_finish:
 *  "question"="answer" joined by commas; `\` and `"` are both escaped
 *  (`\` first, then `"`); unanswered items become Unanswered */
function formatAskReply(questions: QuestionInfo[], answers: (string | null)[]) {
  const esc = (s: string) => s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  return questions
    .map((q, i) => {
      const ans = answers[i]?.trim() || "Unanswered";
      return `"${esc(q.question)}"="${esc(ans)}"`;
    })
    .join(", ");
}

export function AskUserDialog() {
  const askUser = useApp((s) => s.askUser);
  const reply = useApp((s) => s.replyAsk);
  const count = askUser?.questions.length ?? 0;
  // Parent mounts this with key={requestId}: each new round of questions remounts
  // the whole component, so the initial state is this round's questions
  const [answers, setAnswers] = useState<(string | null)[]>(() =>
    Array.from({ length: count }, () => null),
  );
  const [customOpen, setCustomOpen] = useState<boolean[]>(() =>
    Array.from({ length: count }, () => false),
  );

  if (!askUser) return null;
  const questions = askUser.questions;

  /** The answer is custom text (not an option, not empty) */
  const isCustomAnswer = (qi: number) => {
    const v = answers[qi]?.trim();
    return !!v && !questions[qi]!.options.some((o) => o.label === v);
  };

  const submit = () => reply(formatAskReply(questions, answers));

  const pick = (qi: number, label: string) => {
    // Selecting an option is mutually exclusive with a custom answer:
    // set the answer and collapse the custom input
    setAnswers((prev) => {
      const next = [...prev];
      next[qi] = label;
      return next;
    });
    setCustomOpen((prev) => {
      const c = [...prev];
      c[qi] = false;
      return c;
    });
  };

  /** Expands/collapses the custom input; expanding clears a selected option (mutual exclusion) */
  const toggleCustom = (qi: number) => {
    const opening = !customOpen[qi];
    setCustomOpen((prev) => {
      const c = [...prev];
      c[qi] = opening;
      return c;
    });
    if (opening && !isCustomAnswer(qi)) {
      setAnswers((prev) => {
        const next = [...prev];
        next[qi] = null;
        return next;
      });
    }
  };

  /** Custom input change: write the answer in real time */
  const setCustomAnswer = (qi: number, value: string) => {
    setAnswers((prev) => {
      const next = [...prev];
      next[qi] = value;
      return next;
    });
  };

  /** Radio circle: shared by options and custom answers */
  const Circle = ({ filled }: { filled: boolean }) => (
    <span
      className={cn(
        "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border",
        filled
          ? "border-primary bg-primary text-primary-foreground"
          : "border-muted-foreground/40",
      )}
    >
      {filled && <Check className="size-3" />}
    </span>
  );

  const unanswered = answers.some((a) => a === null || !a!.trim());

  return (
    <Overlay onClose={() => reply("[User Cancelled]")} closeOnMask={false}>
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
                    answers[qi] === o.label
                      ? "border-primary/30 bg-primary/[0.06]"
                      : "border-border/50 hover:bg-muted/60",
                  )}
                  onClick={() => pick(qi, o.label)}
                >
                  <Circle filled={answers[qi] === o.label} />
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

              {/* Custom input (matches the TUI's always-appended "Type your own answer") */}
              <button
                type="button"
                className={cn(
                  "flex w-full cursor-pointer items-center gap-2.5 rounded-lg border px-3 py-2 text-left text-sm transition-colors",
                  customOpen[qi] || isCustomAnswer(qi)
                    ? "border-primary/30 bg-primary/[0.06] text-foreground"
                    : "border-border/50 text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
                onClick={() => toggleCustom(qi)}
              >
                <Circle filled={customOpen[qi] || isCustomAnswer(qi)} />
                输入自定义回答
              </button>
              {customOpen[qi] && (
                <input
                  type="text"
                  autoFocus
                  value={isCustomAnswer(qi) ? answers[qi]! : ""}
                  className="w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30"
                  placeholder="输入自定义回答"
                  onChange={(e) => setCustomAnswer(qi, e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      setCustomAnswer(qi, e.currentTarget.value.trim());
                    }
                  }}
                />
              )}
            </div>
          ) : (
            <input
              type="text"
              autoFocus
              value={answers[qi] ?? ""}
              className="w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30"
              placeholder="输入回答"
              onChange={(e) => setCustomAnswer(qi, e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  setCustomAnswer(qi, e.currentTarget.value.trim());
                }
              }}
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
        <button type="button" className={btnPrimary} onClick={submit}>
          提交{unanswered ? "（未答项记为 Unanswered）" : ""}
        </button>
      </div>
    </Overlay>
  );
}

/* ── Workdir picker ───────────────────────────────────────────── */
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

  useEffect(() => {
    if (!show) {
      setPath("");
      setError("");
    }
  }, [show]);

  if (!show) return null;

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

  return (
    <Overlay onClose={() => setWorkdirPicker(false)} wide>
      <h3 className="mb-1 text-[15px] font-semibold">选择项目</h3>
      <p className="mb-4 text-xs text-muted-foreground/60">
        切换到所选目录（skills / AGENTS.md / 权限基准随目录切换），不影响已有会话
      </p>

      {workdirs.length > 0 && (
        <>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">
            最近使用
            {workdirs.length > 5 && (
              <span className="ml-1 text-muted-foreground/50">({workdirs.length})</span>
            )}
          </p>
          <div className="hlx-input-scroll mb-4 max-h-[40vh] space-y-1 overflow-y-auto overscroll-contain py-1 pr-1">
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

      {/* Manual input */}
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
    </Overlay>
  );
}
