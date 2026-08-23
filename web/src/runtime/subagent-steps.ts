// subagent 步骤折叠辅助：动词摘要、结果摘要、task 结果块解析
// （对齐 TUI subagent_step_summary / task_outcomes_from_result）

/** 折叠进 task part 的单条实时步骤（挂在 args._steps） */
export interface TaskStepData {
  /** 来源任务在 tasks 数组中的下标（同名 subagent 并发时区分实例） */
  index: number;
  agent: string;
  text: string;
  done: boolean;
}

/** task 结果中单个任务的产出 */
export interface TaskOutcome {
  agent: string;
  ok: boolean;
  firstLine: string;
  /** 完整输出（task_result / error 内容），供展开查看 */
  full: string;
}

/** 工具动词表（对齐 TUI ToolCategory::verb_done） */
const VERB_DONE: Record<string, string> = {
  read: "Read",
  write: "Wrote",
  edit: "Edited",
  grep: "Searched",
  glob: "Listed",
  bash: "Ran",
  web_fetch: "Fetched",
  todo_write: "Updated",
};

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

function parseArgsLoose(raw: string | undefined): Record<string, any> {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw);
    return typeof v === "object" && v !== null ? (v as Record<string, any>) : {};
  } catch {
    return {};
  }
}

/** subagent 步骤摘要：动词前缀 + 目标（如 "Read src/main.rs"） */
export function subagentStepSummary(name: string, argsText: string | undefined): string {
  const args = parseArgsLoose(argsText);
  let detail = "";
  if (name === "read") {
    const path = typeof args.file_path === "string" ? args.file_path : "";
    const o = args.offset;
    const l = args.limit;
    detail =
      path +
      (o != null && l != null
        ? ` (L${o}-L${Number(o) + Number(l) - 1})`
        : o != null
          ? ` (from L${o})`
          : "");
  } else if (name === "edit") {
    detail = typeof args.file_path === "string" ? args.file_path : "";
    if (args.replace_all === true) detail += " (replace all)";
  } else if (name === "write") {
    detail = typeof args.file_path === "string" ? args.file_path : "";
  } else if (name === "bash") {
    detail = truncate(String(args.command_string ?? ""), 60);
  } else if (name === "web_fetch") {
    detail = String(args.url ?? "");
  } else if (name === "grep") {
    const pattern = String(args.pattern ?? "");
    const path = String(args.path ?? ".");
    const include = typeof args.include === "string" ? args.include : null;
    detail = `"${pattern}" in ${path}${include ? ` (${include})` : ""}`;
  } else if (name === "glob") {
    detail = `${String(args.pattern ?? "")} in ${String(args.path ?? ".")}`;
  } else if (name === "skill") {
    detail = String(args.name ?? "");
  } else if (name.startsWith("mcp__")) {
    // mcp__server__tool → 工具短名
    detail = name.split("__").slice(2).join("__") || name;
  } else if (name !== "todo_write") {
    detail = truncate(argsText ?? "", 40);
  }
  const verb =
    name === "skill" ? "Load skill" : (VERB_DONE[name] ?? name);
  return detail ? `${verb} ${detail}` : verb;
}

function xmlTag(s: string, tag: string): string | null {
  const m = s.match(new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`));
  return m?.[1] ?? null;
}

/** 步骤完成后的结果摘要（单行，追加在步骤文本后，如 " — 42 lines"） */
export function subagentStepResultSummary(name: string, result: string | undefined): string {
  const r = (result ?? "").trim();
  if (!r) return "";
  if (name === "read") {
    const kind = xmlTag(r, "type");
    if (kind === "file") {
      const lines = (xmlTag(r, "content") ?? "").split("\n").length;
      return lines > 0 ? `${lines} lines` : "";
    }
    if (kind === "directory") {
      const entries = (xmlTag(r, "entries") ?? "")
        .split("\n")
        .filter((l) => l.trim()).length;
      return entries > 0 ? `${entries} entries` : "";
    }
    return "";
  }
  if (name === "edit" || name === "write") {
    return r.includes("成功") ? "done" : truncate(r, 40);
  }
  if (name === "bash") {
    // 对齐 Rust tool_result_summary：≤3 行整段截断 120 字符，>3 行取前 3 行 + 计数
    const lines = r.split("\n");
    if (lines.length <= 3) {
      return truncate(r, 120);
    }
    return `${lines.slice(0, 3).join("\n")}\n... +${lines.length - 3} lines`;
  }
  if (name === "grep") {
    if (r.startsWith("No matches found")) return "no matches";
    const files = r.split("\n").filter((l) => l.endsWith(":")).length;
    return files > 0 ? `${files} files` : "";
  }
  if (name === "web_fetch") return `${r.length} chars`;
  return truncate(r.split("\n")[0] ?? "", 40);
}

/** 逐行解析 task 结果中的 `<task ...>` 块（兼容有无 `<tasks>` 包裹），
 *  返回顺序与 tasks 数组一致 */
export function parseTaskOutcomes(result: string): TaskOutcome[] {
  const out: TaskOutcome[] = [];
  let cur: { agent: string; ok: boolean; buf: string[] } | null = null;
  const flush = () => {
    if (!cur) return;
    const full = cur.buf.join("\n").trim();
    out.push({
      agent: cur.agent,
      ok: cur.ok,
      firstLine: full ? (full.split("\n")[0] ?? "") : "(no output)",
      full: full || "(no output)",
    });
    cur = null;
  };
  for (const raw of result.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("<task ")) {
      flush();
      const attr = (k: string) => line.match(new RegExp(`${k}="([^"]*)"`))?.[1];
      cur = {
        agent: attr("subagent") ?? "subagent",
        ok: attr("state") !== "failed",
        buf: [],
      };
    } else if (line.startsWith("</task")) {
      flush();
    } else if (
      cur &&
      line &&
      !line.startsWith("<task_result") &&
      !line.startsWith("<error") &&
      !line.startsWith("<tasks") &&
      !line.startsWith("</")
    ) {
      cur.buf.push(line);
    }
  }
  flush();
  return out;
}
