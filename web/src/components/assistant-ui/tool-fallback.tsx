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

interface DiffChange {
  0: string; // sign: " " | "-" | "+"
  1: string; // text
}

interface DiffHunk {
  old_start: number | null;
  new_start: number | null;
  changes: DiffChange[];
}

interface DiffDisplay {
  format: "diff";
  path: string;
  additions: number;
  deletions: number;
  old_lines?: number;
  new_lines?: number;
  is_new_file?: boolean;
  hunks: DiffHunk[];
}

/** diff 展示（对齐 TUI 渲染：行号 gutter + 符号配色 + hunk 头） */
function DiffView({ display }: { display: string }) {
  let v: DiffDisplay | null = null;
  try {
    const p = JSON.parse(display) as DiffDisplay;
    if (p?.format === "diff" && Array.isArray(p.hunks)) v = p;
  } catch {
    // 忽略解析失败
  }

  // 数据异常（缺 hunks 字段等）：原样文本展示
  if (!v) {
    return (
      <pre className="bg-muted/50 text-foreground/90 mt-1 rounded-md p-2.5 text-xs whitespace-pre-wrap">
        {display}
      </pre>
    );
  }

  const lnWidth = Math.max(
    1,
    String(Math.max(v.old_lines ?? 0, v.new_lines ?? 0)).length,
  );
  const gutter = (ln: number | null) =>
    ln !== null ? String(ln).padStart(lnWidth) : " ".repeat(lnWidth);

  const rows: React.ReactNode[] = [];
  rows.push(
    <div key="stats" className="hlx-diff-hunk">
      {v.path} (+{v.additions} -{v.deletions})
    </div>,
  );

  for (let hi = 0; hi < v.hunks.length; hi++) {
    const h = v.hunks[hi]!;
    const oldLen = h.changes.filter((c) => c[0] !== "+").length;
    const newLen = h.changes.filter((c) => c[0] !== "-").length;
    rows.push(
      <div key={`hunk-${hi}`} className="hlx-diff-hunk">
        @@ -{h.old_start ?? 0},{oldLen} +{h.new_start ?? 0},{newLen} @@
      </div>,
    );
    // 行号推进规则对齐 TUI：delete 走旧行号，insert/context 走新行号
    let oc = h.old_start;
    let nc = h.new_start;
    for (let ci = 0; ci < h.changes.length; ci++) {
      const c = h.changes[ci]!;
      const sign = c[0];
      const text = c[1];
      let ln: number | null;
      if (sign === "-") {
        ln = oc;
        if (oc !== null) oc++;
      } else {
        ln = nc;
        if (nc !== null) nc++;
        if (sign === " " && oc !== null) oc++;
      }
      const cls = sign === "+" ? "hlx-diff-add" : sign === "-" ? "hlx-diff-del" : "";
      rows.push(
        <div key={`hunk-${hi}-${ci}`} className={cls}>
          <span className="text-muted-foreground/70 pr-2 tabular-nums select-none">
            {gutter(ln)}
          </span>
          <span className="whitespace-pre-wrap">
            {sign}
            {text || " "}
          </span>
        </div>,
      );
    }
  }

  return (
    <div className="bg-muted/50 text-foreground/90 mt-1 rounded-md p-2.5 font-mono text-xs overflow-x-auto">
      {rows}
    </div>
  );
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
    return <DiffView display={display} />;
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
