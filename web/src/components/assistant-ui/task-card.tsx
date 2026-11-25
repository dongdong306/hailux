// task tool (subagent delegation) card: live progress while running +
// per-agent results when done
// (aligned with the TUI TaskToolCell; step data is folded into args._steps by
// hailux-runtime)
import { useState } from "react";
import { Check, ChevronDown, Loader2, XCircle } from "lucide-react";
import {
  useToolCallElapsed,
  type ToolCallMessagePartComponent,
} from "@assistant-ui/react";
import { cn } from "../../lib/utils";
import {
  parseTaskOutcomes,
  type TaskStepData,
} from "../../runtime/subagent-steps";

interface TaskItem {
  /** Index into the tasks array (instance identity for concurrent same-named
   *  subagents) */
  index: number;
  subagent: string;
  description: string;
}

function parseTaskItems(args: Record<string, any>): TaskItem[] {
  const arr = args.tasks;
  if (!Array.isArray(arr)) return [{ index: 0, subagent: "subagent", description: "" }];
  return arr.map((t, index) => ({
    index,
    subagent: typeof t?.subagent === "string" ? t.subagent : "subagent",
    description: typeof t?.description === "string" ? t.description : "",
  }));
}

function formatDuration(ms: number) {
  if (ms < 1000) return "<1s";
  const seconds = ms / 1000;
  if (seconds < 10) return `${(Math.floor(seconds * 10) / 10).toFixed(1)}s`;
  if (seconds < 60) return `${Math.floor(seconds)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`;
}

/** Step row: ✓ (done) / spinner (in progress) + summary text */
function StepRow({ step }: { step: TaskStepData }) {
  return (
    <div className="flex items-start gap-2 py-0.5 text-sm leading-relaxed">
      {step.done ? (
        <Check className="text-muted-foreground/60 mt-1 size-3 shrink-0" />
      ) : (
        <Loader2 className="text-muted-foreground/70 mt-1 size-3 shrink-0 animate-spin [animation-duration:0.6s]" />
      )}
      <span className="text-muted-foreground min-w-0 break-words">
        {step.text}
      </span>
    </div>
  );
}

export const TaskToolCard: ToolCallMessagePartComponent = ({
  args,
  argsText,
  result,
  status,
}) => {
  const [open, setOpen] = useState(false);
  const elapsedMs = useToolCallElapsed();
  const statusType = status?.type ?? "complete";
  const isRunning = statusType === "running";

  // args first (includes fold-injected _steps); argsText as fallback (history
  // restore)
  const source =
    args && typeof args === "object" ? args : (() => {
      try {
        const v = JSON.parse(argsText ?? "{}");
        return typeof v === "object" && v !== null
          ? (v as Record<string, any>)
          : {};
      } catch {
        return {};
      }
    })();
  const items = parseTaskItems(source);
  const steps = (Array.isArray(source._steps) ? source._steps : []) as TaskStepData[];
  const resultText =
    (result as { output?: string } | undefined)?.output ??
    (typeof result === "string" ? result : undefined);
  const outcomes = resultText ? parseTaskOutcomes(resultText) : [];
  const multi = items.length > 1;
  const okCount = outcomes.filter((o) => o.ok).length;
  const failCount = outcomes.length - okCount;

  const HeaderIcon = isRunning ? Loader2 : failCount > 0 ? XCircle : Check;

  /** Latest step by task index (concurrent same-named subagents cannot be
   *  told apart by name) */
  const latestStepOf = (index: number): TaskStepData | undefined => {
    for (let i = steps.length - 1; i >= 0; i--) {
      if (steps[i]!.index === index) return steps[i];
    }
    return undefined;
  };

  return (
    <div className="border-border bg-muted/30 my-2 w-full rounded-lg border px-3 py-2">
      {/* Header: status icon + title + count/duration + expand chevron */}
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="hover:text-foreground flex w-full items-center gap-2 py-1.5 text-sm transition-[color,scale] active:scale-[0.98]"
      >
        <HeaderIcon
          className={cn(
            "size-4 shrink-0",
            isRunning && "animate-spin [animation-duration:0.6s]",
            !isRunning && failCount > 0 && "text-destructive",
            !isRunning && failCount === 0 && "text-emerald-600 dark:text-emerald-400",
          )}
        />
        <span className="font-medium leading-none">
          {isRunning ? "正在委派" : "已委派"}
          {multi ? ` ${items.length} 个 agent` : ` ${items[0]?.subagent ?? ""}`}
        </span>
        {!isRunning && multi && outcomes.length > 0 && (
          <span
            className={cn(
              "text-xs leading-none tabular-nums",
              failCount > 0 ? "text-destructive" : "text-muted-foreground/70",
            )}
          >
            {okCount}/{items.length} 成功
          </span>
        )}
        {elapsedMs !== undefined && (
          <span className="text-muted-foreground text-xs leading-none tabular-nums">
            {formatDuration(elapsedMs)}
          </span>
        )}
        <ChevronDown
          className={cn(
            "ml-auto size-4 shrink-0 transition-transform duration-200",
            open ? "rotate-0" : "-rotate-90",
          )}
        />
      </button>

      {/* Overview area (always visible): running = live progress; done =
          first line of each agent's result */}
      <div className="flex flex-col gap-0.5 ps-5 pt-0.5">
        {isRunning
          ? multi
            ? items.map(({ index, subagent, description }) => {
                const step = latestStepOf(index);
                return (
                  <div
                    key={index}
                    className="flex items-center gap-2 py-0.5 text-sm leading-relaxed"
                  >
                    <Loader2 className="text-muted-foreground/70 size-3 shrink-0 animate-spin [animation-duration:0.6s]" />
                    <span className="shrink-0 font-medium">{subagent}</span>
                    <span className="text-muted-foreground min-w-0 flex-1 truncate">
                      {step ? step.text : description || "启动中…"}
                    </span>
                  </div>
                );
              })
            : // Single agent: last 3 steps (task description shown when there
              // are no steps)
              (steps.length > 0
                ? steps.slice(-3).map((step, i) => <StepRow key={i} step={step} />)
                : items[0]?.description && (
                    <div className="text-muted-foreground/70 py-0.5 text-sm">
                      {items[0].description}
                    </div>
                  ))
          : multi
            ? items.map(({ index, subagent }, i) => {
                const o = outcomes[i];
                const matched =
                  o?.agent === subagent ? o : outcomes.find((x) => x.agent === subagent);
                return (
                  <div
                    key={index}
                    className="flex items-center gap-2 py-0.5 text-sm leading-relaxed"
                  >
                    {matched ? (
                      matched.ok ? (
                        <Check className="mt-0.5 size-3 shrink-0 text-emerald-600 dark:text-emerald-400" />
                      ) : (
                        <XCircle className="text-destructive mt-0.5 size-3 shrink-0" />
                      )
                    ) : (
                      <XCircle className="text-muted-foreground/50 mt-0.5 size-3 shrink-0" />
                    )}
                    <span className="shrink-0 font-medium">{subagent}</span>
                    {matched && (
                      <span className="text-muted-foreground/80 min-w-0 flex-1 truncate">
                        {matched.firstLine}
                      </span>
                    )}
                  </div>
                );
              })
            : outcomes[0] && (
                <div className="text-muted-foreground/80 py-0.5 text-sm">
                  {outcomes[0].firstLine}
                </div>
              )}
      </div>

      {/* Expanded area: running = full step log (grouped by agent); done =
          each agent's full result */}
      {open && (
        <div className="mt-2 flex max-h-[60vh] flex-col gap-3 overflow-y-auto border-t border-border/70 ps-5 pt-2">
          {isRunning ? (
            multi ? (
              items.map(({ index, subagent }) => {
                // Step ownership filtered by index (concurrent same-named
                // subagents do not get mixed up);
                // historical data without an index (index=-1) falls back to
                // agent-name matching
                const own = steps.filter(
                  (s) => (index >= 0 ? s.index === index : s.agent === subagent),
                );
                return (
                  <div key={index}>
                    <div className="text-muted-foreground/70 mb-0.5 text-xs font-medium">
                      {subagent}
                    </div>
                    {own.length > 0 ? (
                      own.map((step, i) => <StepRow key={i} step={step} />)
                    ) : (
                      <div className="text-muted-foreground/50 text-sm">
                        （暂无记录）
                      </div>
                    )}
                  </div>
                );
              })
            ) : (
              steps.map((step, i) => <StepRow key={i} step={step} />)
            )
          ) : outcomes.length > 0 ? (
            outcomes.map((o, i) => (
              <div key={`${o.agent}-${i}`}>
                <div className="mb-0.5 flex items-center gap-1.5 text-xs font-medium">
                  {o.ok ? (
                    <Check className="size-3 text-emerald-600 dark:text-emerald-400" />
                  ) : (
                    <XCircle className="text-destructive size-3" />
                  )}
                  <span>{o.agent}</span>
                </div>
                <pre className="bg-muted/50 text-foreground/90 rounded-md p-2.5 text-xs whitespace-pre-wrap">
                  {o.full}
                </pre>
              </div>
            ))
          ) : (
            resultText && (
              <pre className="bg-muted/50 text-foreground/90 rounded-md p-2.5 text-xs whitespace-pre-wrap">
                {resultText}
              </pre>
            )
          )}
        </div>
      )}
    </div>
  );
};
