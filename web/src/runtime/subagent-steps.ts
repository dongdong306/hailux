// subagent step folding helpers: verb summaries, result summaries, task
// result block parsing
// (aligned with the TUI's subagent_step_summary / task_outcomes_from_result)

/** One live step folded into the task part (attached at args._steps) */
export interface TaskStepData {
  /** Index of the source task in the tasks array (tells apart concurrent
   *  same-named subagents) */
  index: number;
  agent: string;
  text: string;
  done: boolean;
}

/** One task's outcome within the task result */
export interface TaskOutcome {
  agent: string;
  ok: boolean;
  firstLine: string;
  /** Full output (task_result / error content), for the expanded view */
  full: string;
}

/** Tool verb table (aligned with the TUI's ToolCategory::verb_done) */
const VERB_DONE: Record<string, string> = {
  read: "Read",
  write: "Wrote",
  edit: "Edited",
  grep: "Searched",
  glob: "Listed",
  shell: "Ran",
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

/** Subagent step summary: verb prefix + target (e.g. "Read src/main.rs") */
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
  } else if (name === "shell") {
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
    // mcp__server__tool → short tool name
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

/** Result summary after a step completes (single line, appended after the
 *  step text, e.g. " — 42 lines") */
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
  if (name === "shell") {
    // Aligned with the Rust tool_result_summary: ≤3 lines truncate the whole
    // text to 120 chars; >3 lines take the first 3 lines + a count
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

/** Parse the `<task ...>` blocks in a task result line by line (tolerates
 *  presence or absence of a `<tasks>` wrapper),
 *  returns in the same order as the tasks array */
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
