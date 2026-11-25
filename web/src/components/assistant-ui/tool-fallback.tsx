// Tool calls: assistant-ui official base styling (ToolFallback), adapted to
// hailux result.output/display
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
import { useApp } from "../../store/app-store";

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

/** Diff view (aligned with TUI rendering: line-number gutter + sign colors +
 *  hunk headers) */
function DiffView({ display }: { display: string }) {
  let v: DiffDisplay | null = null;
  try {
    const p = JSON.parse(display) as DiffDisplay;
    if (p?.format === "diff" && Array.isArray(p.hunks)) v = p;
  } catch {
    // Ignore parse failures
  }

  // Malformed data (missing hunks field etc.): show the raw text
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
    // Line-number advance rule aligned with the TUI: deletes use old line
    // numbers, insert/context use new ones
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

/** todo_write view (aligned with the TUI TodoCell: checkbox-style list, no
 *  raw args/result shown) */
function TodoView({ argsText, running }: { argsText: string; running: boolean }) {
  let todos: TodoItemData[] = [];
  try {
    const args = JSON.parse(argsText) as { todos?: TodoItemData[] };
    if (Array.isArray(args.todos)) todos = args.todos;
  } catch {
    // Args may be incomplete while streaming; render an empty list for now
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
          {running && !allDone ? "Updating" : "Updated"} todo
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

/** Builtin tool set (src/agent/tools): titles get no "使用工具：" prefix */
const BUILTIN_TOOLS = new Set([
  "shell",
  "read",
  "edit",
  "write",
  "grep",
  "glob",
  "web_fetch",
  "todo_write",
  "question",
  "skill",
  "task",
]);

/** File display name: inside the work dir → relative path; outside → full
 *  path.
 *  Normalize `\`→`/` then do a case-insensitive prefix comparison (Windows);
 *  the prefix has trailing separators stripped to avoid matching sibling dirs */
function displayPath(filePath: string, workDir: string): string {
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const wd = norm(workDir);
  if (!wd) return filePath;
  if (norm(filePath).toLowerCase().startsWith(`${wd.toLowerCase()}/`)) {
    const rel = filePath.slice(wd.length);
    return rel.replace(/^[\\/]+/, "") || "./";
  }
  return filePath;
}

/** Parses "question"="answer" pairs from a question result (mirrors TUI
 *  parse_ask_user_pairs: quote scanning + `\\`/`\"` unescaping + key=value pairs) */
function parseAskUserPairs(result: string): [string, string][] {
  const pairs: [string, string][] = [];
  let i = 0;
  const readQuoted = (): string | null => {
    if (result[i] !== '"') return null;
    let j = i + 1;
    let val = "";
    while (j < result.length) {
      if (result[j] === "\\") {
        // `\\`→`\`, `\"`→`"`; lone backslashes (bad input/old data) stay as-is
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
      // Skip whitespace looking for =
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

/** question result view (mirrors TUI render_ask_user_result: Q&A list / cancelled greyed out) */
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

/** Row-render marker inside the merged consecutive-tool-calls card: embedded
 *  mode drops the outer card border */
const ToolGroupContext = createContext(false);

/** Merged card for consecutive tool calls: the header shows the call count,
 *  the body holds collapsible rows per tool */
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
  // Expanded by default while running, collapsed once done; after the user
  // toggles manually, respect that manual state
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
  // Subscription read: called before the todo_write/question early returns, keeping hook order stable
  const workDir = useApp((s) => s.workDir);
  const statusType = status?.type ?? "complete";
  const isRunning = statusType === "running";

  // todo_write: no raw args/result shown, rendered as a checkbox-style todo list
  if (toolName === "todo_write") {
    return <TodoView argsText={argsText ?? ""} running={isRunning} />;
  }

  if (toolName === "question") {
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

  // Arg parsing (may be incomplete while streaming; fall back to empty)
  let args: Record<string, any> = {};
  try {
    args = JSON.parse(argsText ?? "{}") as Record<string, any>;
  } catch {
    // Args may be incomplete while streaming
  }

  // Title: builtin tools get no "使用工具：" prefix. Structure = verb (small) + object (main).
  // File tools (edit/write/read) show the path: in-workdir → relative, outside → full;
  // grep/glob show pattern + search scope (path/include);
  // skill mirrors the TUI's "Load skill {name}"; other builtin tools take a
  // summary by field priority (shell.command_string / web_fetch.url / task.description);
  // unknown tools keep the prefix.
  // Missing object (args not streamed yet / parse failure) shows the verb only.
  let verb: string;
  let object: string | null;
  let suffix: string | null = null;
  if (toolName === "edit" || toolName === "write" || toolName === "read") {
    verb = toolName;
    const filePath = typeof args.file_path === "string" ? args.file_path : "";
    object = filePath ? displayPath(filePath, workDir) : null;
  } else if (toolName === "skill") {
    verb = "Load skill";
    object = typeof args.name === "string" ? args.name : null;
  } else if (toolName === "grep" || toolName === "glob") {
    verb = toolName;
    object = typeof args.pattern === "string" ? args.pattern : null;
    // Search scope: path (grep/glob) or include (grep file filter), shown
    // only when non-default
    const scope =
      typeof args.path === "string" && args.path && args.path !== "."
        ? args.path
        : typeof args.include === "string" && args.include
          ? args.include
          : null;
    if (scope) suffix = scope;
  } else if (BUILTIN_TOOLS.has(toolName)) {
    // Other builtin tools: take a summary by explicit field priority, not
    // JSON key order
    verb = toolName;
    object =
      (typeof args.command_string === "string" && args.command_string) ||
      (typeof args.url === "string" && args.url) ||
      (typeof args.description === "string" && args.description) ||
      null;
  } else {
    verb = "使用工具";
    object = toolName;
  }
  const title: ReactNode = (
    <>
      <span className="text-xs leading-none">{verb}</span>
      {object && (
        <span className="min-w-0 truncate font-medium leading-none">
          {object}
        </span>
      )}
      {suffix && (
        <span className="text-muted-foreground/70 hidden truncate text-xs leading-none sm:inline">
          {suffix}
        </span>
      )}
    </>
  );

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
        <span className="flex min-w-0 flex-1 items-baseline gap-1.5 overflow-hidden">
          {title}
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

      {/* Expanded content height cap (long output must not blow up the page);
          scroll internally beyond it */}
      {open && (
        <div className="flex max-h-[60vh] flex-col gap-2 overflow-y-auto ps-6 pt-1 pb-2">
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
