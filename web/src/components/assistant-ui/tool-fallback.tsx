// 工具调用：assistant-ui 官方 base 样式（ToolFallback），适配 hailux result.output/display
import { createContext, useContext, useState, type ReactNode } from "react";
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

interface TodoItemData {
  content?: string;
  status?: string;
  priority?: string;
}

const TODO_ICONS: Record<string, { icon: string; className: string }> = {
  completed: { icon: "✓", className: "text-emerald-600 dark:text-emerald-400" },
  in_progress: { icon: "→", className: "text-cyan-600 dark:text-cyan-400" },
  pending: { icon: "○", className: "text-muted-foreground/60" },
  cancelled: { icon: "✗", className: "text-muted-foreground/60" },
};

/** todo_write 展示（对齐 TUI TodoCell：checkbox 风格列表，不展示入参/出参原文） */
function TodoView({ argsText, running }: { argsText: string; running: boolean }) {
  let todos: TodoItemData[] = [];
  try {
    const args = JSON.parse(argsText) as { todos?: TodoItemData[] };
    if (Array.isArray(args.todos)) todos = args.todos;
  } catch {
    // 流式期间入参可能不完整，先按空列表渲染
  }

  const done = todos.filter(
    (t) => t.status === "completed" || t.status === "cancelled",
  ).length;
  const allDone = todos.length > 0 && done === todos.length;

  return (
    <div className="border-border bg-muted/30 my-2 w-full rounded-lg border px-3 py-2 text-sm">
      <div className="flex items-center gap-2 py-1 leading-none">
        <span
          className={cn(
            allDone
              ? "text-emerald-600 dark:text-emerald-400 font-bold"
              : "text-cyan-600 dark:text-cyan-400",
          )}
        >
          •
        </span>
        <span className="font-medium">
          {running && !allDone ? "Updating" : "Updated"} plan
        </span>
        {todos.length > 0 && (
          <span className="text-muted-foreground/70 text-xs tabular-nums">
            {done}/{todos.length}
          </span>
        )}
      </div>
      <div className="flex flex-col gap-0.5 ps-4 pt-0.5">
        {todos.map((t, i) => {
          const meta = TODO_ICONS[t.status ?? "pending"] ?? TODO_ICONS.pending!;
          const isDone = t.status === "completed";
          const isCancelled = t.status === "cancelled";
          return (
            <div key={i} className="flex items-start gap-2 leading-relaxed">
              <span className={`${meta.className} shrink-0 select-none`} aria-hidden>
                {meta.icon}
              </span>
              <span
                className={cn(
                  "min-w-0 break-words",
                  (isDone || isCancelled) &&
                    "text-muted-foreground/70 line-through",
                  t.status === "in_progress" && "text-foreground font-medium",
                )}
              >
                {t.content || "—"}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const statusIconMap = {
  running: Loader2,
  complete: Check,
  incomplete: XCircle,
  "requires-action": AlertCircle,
} as const;

/** 从 ask_user 结果解析 "question"="answer" 对（对齐 TUI parse_ask_user_pairs：
 *  引号扫描 + `\\`/`\"` 转义还原 + key=value 配对） */
function parseAskUserPairs(result: string): [string, string][] {
  const pairs: [string, string][] = [];
  let i = 0;
  const readQuoted = (): string | null => {
    if (result[i] !== '"') return null;
    let j = i + 1;
    let val = "";
    while (j < result.length) {
      if (result[j] === "\\") {
        // `\\`→`\`、`\"`→`"`；孤立反斜杠（异常输入/旧数据）原样保留
        const next = result[j + 1];
        if (next === "\\" || next === '"') {
          val += next;
          j += 2;
          continue;
        }
      }
      if (result[j] === '"') break;
      val += result[j];
      j++;
    }
    if (j >= result.length) return null;
    i = j + 1;
    return val;
  };
  while (i < result.length) {
    if (result[i] === '"') {
      const key = readQuoted();
      if (key === null) break;
      // 跳过空白找 =
      while (i < result.length && result[i] === " ") i++;
      if (result[i] !== "=") continue;
      i++;
      while (i < result.length && result[i] === " ") i++;
      const val = readQuoted();
      if (val !== null) pairs.push([key, val]);
    } else {
      i++;
    }
  }
  return pairs;
}

/** ask_user 结果展示（对齐 TUI render_ask_user_result：Q&A 列表 / cancelled 灰显） */
function AskUserView({ result }: { result: string }) {
  if (result.includes("[User Cancelled]")) {
    return (
      <div className="text-muted-foreground/60 my-1 text-sm">cancelled</div>
    );
  }
  const pairs = parseAskUserPairs(result);
  return (
    <div className="my-1 flex flex-col gap-1">
      {pairs.length === 0 ? (
        <div className="text-muted-foreground/60 text-sm">Unanswered</div>
      ) : (
        pairs.map(([q, a], i) => (
          <div key={i} className="text-sm leading-relaxed">
            <span className="text-muted-foreground/70">{q}</span>
            <span className="text-muted-foreground/40"> → </span>
            <span className="text-foreground font-medium">{a}</span>
          </div>
        ))
      )}
    </div>
  );
}

/** 连续工具调用合并卡片内的行渲染标记：嵌入模式去掉外层卡片边框 */
const ToolGroupContext = createContext(false);

/** 连续工具调用合并卡片：头部展示调用次数，内部为各工具的折叠行 */
export function ToolGroup({
  count,
  running,
  children,
}: {
  count: number;
  running: boolean;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [userToggled, setUserToggled] = useState(false);
  // 运行中默认展开，结束后收起；用户手动切换后尊重手动状态
  const effectiveOpen = userToggled ? open : running;
  const Icon = running ? Loader2 : Check;

  return (
    <div className="border-border my-2 w-full rounded-lg border px-3 py-2">
      <button
        type="button"
        onClick={() => {
          setUserToggled(true);
          setOpen(!effectiveOpen);
        }}
        className="text-muted-foreground hover:text-foreground flex w-full items-center gap-2 py-1.5 text-sm transition-[color,scale] active:scale-[0.98]"
      >
        <Icon
          className={cn(
            "size-4 shrink-0",
            running && "animate-spin [animation-duration:0.6s]",
          )}
        />
        <span className="min-w-0 truncate leading-none">
          {running ? "正在执行" : "执行了"} {count} 次工具调用
        </span>
        <ChevronDown
          className={cn(
            "ml-auto size-4 shrink-0 transition-transform duration-200",
            effectiveOpen ? "rotate-0" : "-rotate-90",
          )}
        />
      </button>

      {effectiveOpen && (
        <div className="divide-y divide-border/70 pt-1 pb-1">
          <ToolGroupContext.Provider value={true}>
            {children}
          </ToolGroupContext.Provider>
        </div>
      )}
    </div>
  );
}

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
  const inGroup = useContext(ToolGroupContext);
  const [open, setOpen] = useState(false);
  const elapsedMs = useToolCallElapsed();
  const statusType = status?.type ?? "complete";
  const isRunning = statusType === "running";

  // todo_write：不展示入参/出参，渲染为 checkbox 风格 todo 列表
  if (toolName === "todo_write") {
    return <TodoView argsText={argsText ?? ""} running={isRunning} />;
  }

  // ask_user：运行中显示 Asking，完成后渲染 Q&A 列表（不展示原始入参/出参）
  if (toolName === "ask_user") {
    const text = (result as ToolOutput | undefined)?.output;
    return (
      <div
        className={cn(
          "my-2 w-full rounded-lg border px-3 py-2 text-sm",
          inGroup ? "border-border/50" : "border-border",
        )}
      >
        <div className="flex items-center gap-2 py-1 leading-none">
          {isRunning ? (
            <Loader2 className="text-muted-foreground size-4 shrink-0 animate-spin [animation-duration:0.6s]" />
          ) : (
            <Check className="text-muted-foreground size-4 shrink-0" />
          )}
          <span className="font-medium">
            {isRunning ? "正在提问" : "已提问"}
          </span>
        </div>
        {!isRunning && text && <AskUserView result={text} />}
      </div>
    );
  }

  const Icon = statusIconMap[statusType as keyof typeof statusIconMap] ?? Check;

  return (
    <div
      className={
        inGroup
          ? "w-full py-1"
          : "border-border my-2 w-full rounded-lg border px-3 py-2"
      }
    >
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
