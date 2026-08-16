// 工具调用：assistant-ui 官方 base 样式（ToolFallback），适配 hailux result.output/display
import { useState } from "react";
import {
  AlertCircle,
  Check,
  ChevronDown,
  Loader2,
  XCircle,
} from "lucide-react";
import {
  useToolCallElapsed,
  type ToolCallMessagePartComponent,
} from "@assistant-ui/react";
import { cn } from "../../lib/utils";

interface ToolOutput {
  output?: string;
  display?: string;
}

const statusIconMap = {
  running: Loader2,
  complete: Check,
  incomplete: XCircle,
  "requires-action": AlertCircle,
} as const;

function formatDuration(ms: number) {
  if (ms < 1000) return "<1s";
  const seconds = ms / 1000;
  if (seconds < 10) return `${(Math.floor(seconds * 10) / 10).toFixed(1)}s`;
  if (seconds < 60) return `${Math.floor(seconds)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`;
}

function ResultBody({ result }: { result: unknown }) {
  const display = (result as ToolOutput | undefined)?.display;
  if (display) {
    return (
      <pre className="bg-muted/50 text-foreground/90 mt-1 rounded-md p-2.5 text-xs whitespace-pre-wrap">
        {display.split("\n").map((line, i) => {
          const cls = line.startsWith("+")
            ? "hlx-diff-add"
            : line.startsWith("-")
              ? "hlx-diff-del"
              : line.startsWith("@@")
                ? "hlx-diff-hunk"
                : "";
          return (
            <span key={i} className={cls}>
              {line || " "}
            </span>
          );
        })}
      </pre>
    );
  }
  const text =
    (result as ToolOutput | undefined)?.output ??
    (typeof result === "string" ? result : JSON.stringify(result, null, 2));
  return (
    <pre className="bg-muted/50 text-foreground/90 mt-1 rounded-md p-2.5 text-xs whitespace-pre-wrap">
      {text}
    </pre>
  );
}

export const ToolFallback: ToolCallMessagePartComponent = ({
  toolName,
  argsText,
  result,
  status,
}) => {
  const [open, setOpen] = useState(false);
  const elapsedMs = useToolCallElapsed();
  const statusType = status?.type ?? "complete";
  const isRunning = statusType === "running";

  const Icon = statusIconMap[statusType as keyof typeof statusIconMap] ?? Check;

  return (
    <div className="my-2 w-full rounded-lg border border-border px-3 py-2">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="text-muted-foreground hover:text-foreground flex w-full items-center gap-2 py-1.5 text-sm transition-[color,scale] active:scale-[0.98]"
      >
        <Icon
          className={cn(
            "size-4 shrink-0",
            isRunning && "animate-spin [animation-duration:0.6s]",
          )}
        />
        <span className="min-w-0 truncate leading-none">
          使用工具：<b>{toolName}</b>
        </span>
        {elapsedMs !== undefined && (
          <span className="text-muted-foreground text-xs tabular-nums">
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

      {open && (
        <div className="flex flex-col gap-2 ps-6 pt-1 pb-2">
          {argsText && (
            <pre className="bg-muted/50 text-foreground/90 rounded-md p-2.5 text-xs whitespace-pre-wrap">
              {argsText}
            </pre>
          )}
          {result !== undefined && (
            <div>
              <p className="text-muted-foreground text-xs font-medium">结果：</p>
              <ResultBody result={result} />
            </div>
          )}
        </div>
      )}
    </div>
  );
};
