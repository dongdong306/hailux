// 技能管理面板：内联双栏视图（左列表 + 右详情/编辑器），替换聊天区渲染。
// 支持新建/编辑/删除/搜索/作用域过滤；删除二次确认用小弹窗。
import { useEffect, useMemo, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Folder,
  Globe,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import {
  useApp,
  type SkillEntry,
  type SkillFileEntry,
} from "../store/app-store";
import { Overlay } from "./dialogs";
import { cn } from "../lib/utils";

type ScopeFilter = "all" | "global" | "project";

type Editor = { mode: "create" } | { mode: "edit"; skill: SkillEntry };

function ScopeBadge({ scope }: { scope: "global" | "project" }) {
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-0.5 rounded-full px-1.5 py-px text-[10px] font-medium",
        scope === "global"
          ? "bg-primary/10 text-primary/80"
          : "bg-muted text-muted-foreground/70",
      )}
    >
      {scope === "global" ? (
        <Globe className="size-2.5" />
      ) : (
        <Folder className="size-2.5" />
      )}
      {scope === "global" ? "全局" : "项目"}
    </span>
  );
}

/* ── 新建 / 编辑表单 ────────────────────────────────────────── */
function SkillForm({
  editor,
  onDone,
  onCancel,
}: {
  editor: Editor;
  onDone: (name: string) => void;
  onCancel: () => void;
}) {
  const createSkill = useApp((s) => s.createSkill);
  const updateSkill = useApp((s) => s.updateSkill);
  const isCreate = editor.mode === "create";
  const [name, setName] = useState(isCreate ? "" : editor.skill.name);
  const [description, setDescription] = useState(
    isCreate ? "" : editor.skill.description,
  );
  const [scope, setScope] = useState<"global" | "project">("global");
  const [content, setContent] = useState(isCreate ? "" : editor.skill.content);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const submit = async () => {
    if (busy) return;
    if (!name.trim()) {
      setError("请输入技能名（字母、数字、-、_）");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const savedName = name.trim();
      if (isCreate) {
        await createSkill({
          name: savedName,
          description: description.trim(),
          content,
          scope,
        });
      } else {
        await updateSkill({
          location: editor.skill.location,
          name: savedName,
          description: description.trim(),
          content,
        });
      }
      onDone(savedName);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const inputCls =
    "w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30";

  return (
    <div className="flex h-full flex-col">
      <div className="mb-4 flex items-center gap-2.5">
        <span className="flex size-8 items-center justify-center rounded-lg bg-primary/10 ring-1 ring-primary/20">
          <Pencil className="size-4 text-primary" />
        </span>
        <h3 className="text-[15px] font-semibold">
          {isCreate ? "新建技能" : "编辑技能"}
        </h3>
      </div>

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto pr-1">
        <div>
          <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
            名称
          </label>
          {isCreate ? (
            <input
              value={name}
              autoFocus
              placeholder="my-skill"
              className={cn(inputCls, "font-mono")}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submit()}
            />
          ) : (
            <div className="rounded-lg border border-border/50 bg-muted/30 px-3 py-2 font-mono text-sm text-foreground/80">
              {name}
            </div>
          )}
          <p className="mt-1 text-xs text-muted-foreground/60">
            只能包含字母、数字、-、_，且以字母或数字开头
          </p>
        </div>

        <div>
          <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
            描述
          </label>
          <input
            value={description}
            placeholder="何时使用此技能（注入 system prompt 的摘要）"
            className={inputCls}
            onChange={(e) => setDescription(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
        </div>

        {isCreate && (
          <div>
            <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
              作用域
            </label>
            <div className="flex gap-2">
              {(["global", "project"] as const).map((sc) => (
                <label
                  key={sc}
                  className={cn(
                    "flex flex-1 cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
                    scope === sc
                      ? "border-primary/30 bg-primary/[0.06] text-foreground"
                      : "border-border/50 text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <input
                    type="radio"
                    name="skill-scope"
                    className="accent-primary"
                    checked={scope === sc}
                    onChange={() => setScope(sc)}
                  />
                  {sc === "global" ? (
                    <>
                      <Globe className="size-3.5" />
                      全局（~/.hailux/skills/）
                    </>
                  ) : (
                    <>
                      <Folder className="size-3.5" />
                      项目（./.hailux/skills/）
                    </>
                  )}
                </label>
              ))}
            </div>
          </div>
        )}

        <div className="flex min-h-0 flex-1 flex-col">
          <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
            正文（Markdown 指令）
          </label>
          <textarea
            value={content}
            placeholder={"# 指令正文\n\n加载该技能时注入给模型的完整内容"}
            className={cn(inputCls, "hlx-input-scroll min-h-64 flex-1 resize-none font-mono text-[13px] leading-relaxed")}
            onChange={(e) => setContent(e.target.value)}
          />
        </div>
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <div className="mt-4 flex justify-end gap-2 border-t border-border/40 pt-4">
        <button
          type="button"
          className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={onCancel}
          disabled={busy}
        >
          取消
        </button>
        <button
          type="button"
          className="flex cursor-pointer items-center gap-1.5 rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30"
          onClick={submit}
          disabled={busy}
        >
          {busy && <Loader2 className="size-3.5 animate-spin" />}
          保存
        </button>
      </div>
    </div>
  );
}

/* ── 技能详情 ───────────────────────────────────────────────── */
/** 文件大小格式化（B / KB / MB） */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 文件树节点：文件夹（可折叠）或文件 */
type TreeNode =
  | { kind: "dir"; name: string; path: string; children: Map<string, TreeNode> }
  | { kind: "file"; name: string; path: string; size: number };

/** 由扁平相对路径列表构建目录树 */
function buildTree(files: SkillFileEntry[]): Map<string, TreeNode> {
  const root = new Map<string, TreeNode>();
  for (const f of files) {
    if (f.path === "SKILL.md") continue;
    const parts = f.path.split("/");
    let children = root;
    for (let i = 0; i < parts.length; i++) {
      const name = parts[i]!;
      const path = parts.slice(0, i + 1).join("/");
      if (i === parts.length - 1) {
        children.set(name, { kind: "file", name, path, size: f.size });
      } else {
        let node = children.get(name);
        if (!node || node.kind !== "dir") {
          node = { kind: "dir", name, path, children: new Map() };
          children.set(name, node);
        }
        children = node.children;
      }
    }
  }
  return root;
}

/** 目录内文件总数（含子目录，用于文件夹行显示） */
function countFiles(node: TreeNode): number {
  if (node.kind === "file") return 1;
  let n = 0;
  for (const child of node.children.values()) n += countFiles(child);
  return n;
}

/** 递归渲染文件树 */
function TreeView({
  children,
  depth,
  activeFile,
  collapsed,
  onToggle,
  onOpen,
}: {
  children: Map<string, TreeNode>;
  depth: number;
  activeFile: string | null;
  collapsed: Set<string>;
  onToggle: (path: string) => void;
  onOpen: (path: string) => void;
}) {
  // 文件夹在前、各自按名称排序
  const nodes = [...children.values()].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });

  return (
    <>
      {nodes.map((node) =>
        node.kind === "dir" ? (
          <div key={node.path}>
            <button
              type="button"
              className="flex w-full cursor-pointer items-center gap-1 rounded-md py-1 pr-2 text-left text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
              style={{ paddingLeft: `${depth * 0.85}rem` }}
              onClick={() => onToggle(node.path)}
            >
              {collapsed.has(node.path) ? (
                <ChevronRight className="size-3 shrink-0 text-muted-foreground/40" />
              ) : (
                <ChevronDown className="size-3 shrink-0 text-muted-foreground/40" />
              )}
              <Folder className="size-3.5 shrink-0 text-primary/70" />
              <span className="min-w-0 flex-1 truncate font-medium">
                {node.name}
              </span>
              <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground/40">
                {countFiles(node)}
              </span>
            </button>
            {!collapsed.has(node.path) && (
              <TreeView
                children={node.children}
                depth={depth + 1}
                activeFile={activeFile}
                collapsed={collapsed}
                onToggle={onToggle}
                onOpen={onOpen}
              />
            )}
          </div>
        ) : (
          <button
            key={node.path}
            type="button"
            title={`${node.path} · ${formatSize(node.size)}`}
            className={cn(
              "flex w-full cursor-pointer items-center gap-1 rounded-md py-1 pr-2 text-left font-mono text-xs transition-colors",
              activeFile === node.path
                ? "bg-primary/[0.08] font-medium text-primary"
                : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
            style={{ paddingLeft: `${depth * 0.85 + 1.1}rem` }}
            onClick={() => onOpen(node.path)}
          >
            <FileText className="size-3.5 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{node.name}</span>
            <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground/40">
              {formatSize(node.size)}
            </span>
          </button>
        ),
      )}
    </>
  );
}

function SkillDetail({
  skill,
  onEdit,
  onDelete,
}: {
  skill: SkillEntry;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const workDir = useApp((s) => s.workDir);
  // null = SKILL.md 正文；其他值为技能目录内相对路径
  const [activeFile, setActiveFile] = useState<string | null>(null);
  const [fileContent, setFileContent] = useState("");
  const [fileError, setFileError] = useState("");
  const [fileLoading, setFileLoading] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  // 切换技能时重置文件视图
  useEffect(() => {
    setActiveFile(null);
    setFileContent("");
    setFileError("");
    setCollapsed(new Set());
  }, [skill.location]);

  const tree = useMemo(() => buildTree(skill.files), [skill.files]);
  const hasTree = tree.size > 0;

  const openFile = async (path: string | null) => {
    setActiveFile(path);
    setFileError("");
    setFileContent("");
    if (path === null) return;
    setFileLoading(true);
    try {
      const params = new URLSearchParams({
        location: skill.location,
        file: path,
      });
      if (workDir) params.set("work_dir", workDir);
      const resp = await fetch(`/api/skills/file?${params.toString()}`);
      if (!resp.ok) {
        throw new Error(await resp.text().catch(() => `HTTP ${resp.status}`));
      }
      const data = (await resp.json()) as { content: string };
      setFileContent(data.content);
    } catch (e) {
      setFileError(e instanceof Error ? e.message : String(e));
    } finally {
      setFileLoading(false);
    }
  };

  const toggleDir = (path: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const activeSize = skill.files.find((f) => f.path === activeFile)?.size;

  return (
    <div className="flex h-full flex-col">
      <div className="mb-1 flex items-start gap-2.5">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate font-mono text-base font-semibold text-foreground">
              {skill.name}
            </h3>
            <ScopeBadge scope={skill.scope} />
          </div>
          {skill.description && (
            <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
              {skill.description}
            </p>
          )}
        </div>
        <div className="flex shrink-0 gap-1.5">
          <button
            type="button"
            className="flex cursor-pointer items-center gap-1 rounded-lg border border-border px-2.5 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            onClick={onEdit}
          >
            <Pencil className="size-3" />
            编辑
          </button>
          <button
            type="button"
            className="flex cursor-pointer items-center gap-1 rounded-lg border border-destructive/50 px-2.5 py-1.5 text-xs text-destructive transition-colors hover:bg-destructive/10"
            onClick={onDelete}
          >
            <Trash2 className="size-3" />
            删除
          </button>
        </div>
      </div>

      <div className="mb-3 flex items-center gap-1.5 text-xs text-muted-foreground/60">
        <FileText className="size-3 shrink-0" />
        <span className="truncate font-mono" title={skill.location}>
          {skill.location}
        </span>
      </div>

      {/* 主体：无附属文件时直接展示 SKILL.md 正文；有则左文件树 + 右内容 */}
      {!hasTree ? (
        <pre className="hlx-input-scroll min-h-0 flex-1 overflow-y-auto rounded-lg border border-border/50 p-3 text-xs leading-relaxed">
          {skill.content || "（无正文）"}
        </pre>
      ) : (
        <div className="flex min-h-0 flex-1 gap-2">
          {/* 左：文件树（文件夹可折叠） */}
          <div className="hlx-input-scroll w-56 shrink-0 overflow-y-auto rounded-lg border border-border/50 p-1.5">
            <button
              type="button"
              className={cn(
                "flex w-full cursor-pointer items-center gap-1 rounded-md px-2 py-1.5 text-left font-mono text-xs transition-colors",
                activeFile === null
                  ? "bg-primary/[0.08] font-medium text-primary"
                  : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              )}
              onClick={() => openFile(null)}
            >
              <FileText className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate">SKILL.md</span>
            </button>
            <TreeView
              children={tree}
              depth={0}
              activeFile={activeFile}
              collapsed={collapsed}
              onToggle={toggleDir}
              onOpen={openFile}
            />
          </div>

          {/* 右：文件内容 */}
          <div className="flex min-w-0 flex-1 flex-col">
            <div className="mb-1 flex shrink-0 items-center justify-between rounded-lg bg-muted/40 px-2.5 py-1 text-xs text-muted-foreground/70">
              <span className="truncate font-mono">
                {activeFile ?? "SKILL.md"}
              </span>
              <span className="shrink-0 tabular-nums">
                {activeFile !== null && activeSize !== undefined
                  ? formatSize(activeSize)
                  : ""}
              </span>
            </div>
            {fileLoading ? (
              <div className="flex flex-1 items-center justify-center gap-2 text-xs text-muted-foreground/60">
                <Loader2 className="size-3.5 animate-spin" />
                读取中…
              </div>
            ) : fileError ? (
              <p className="flex flex-1 items-center justify-center rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-center text-xs leading-relaxed text-destructive">
                {fileError}
              </p>
            ) : (
              <pre className="hlx-input-scroll min-h-0 flex-1 overflow-y-auto rounded-lg border border-border/50 p-3 text-xs leading-relaxed">
                {activeFile === null
                  ? (skill.content || "（无正文）")
                  : (fileContent || "（空文件）")}
              </pre>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/* ── 主面板 ─────────────────────────────────────────────────── */
export function SkillsManager() {
  const skills = useApp((s) => s.skills);
  const skillsError = useApp((s) => s.skillsError);
  const reloadSkills = useApp((s) => s.reloadSkills);
  const setView = useApp((s) => s.setView);
  const [query, setQuery] = useState("");
  const [scopeFilter, setScopeFilter] = useState<ScopeFilter>("all");
  const [selected, setSelected] = useState<string | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [deleting, setDeleting] = useState<SkillEntry | null>(null);
  const [deletingBusy, setDeletingBusy] = useState(false);
  const [deletingError, setDeletingError] = useState("");
  const [pendingSelect, setPendingSelect] = useState<string | null>(null);

  const close = () => {
    setView("chat");
    setQuery("");
    setScopeFilter("all");
    setSelected(null);
    setEditor(null);
    setDeleting(null);
    setPendingSelect(null);
  };

  // Esc：编辑/删除中先退出当前子状态，否则回到聊天视图
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (deleting) return; // 删除确认弹窗自行处理
      if (editor) setEditor(null);
      else close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor, deleting]);

  // 保存后按名称选中新建/编辑的技能（创建时 location 尚不可知）
  useEffect(() => {
    if (!pendingSelect) return;
    const hit = skills.find((s) => s.name === pendingSelect);
    if (hit) {
      setSelected(hit.location);
      setPendingSelect(null);
    }
  }, [skills, pendingSelect]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return skills.filter(
      (s) =>
        (scopeFilter === "all" || s.scope === scopeFilter) &&
        (!q ||
          s.name.toLowerCase().includes(q) ||
          s.description.toLowerCase().includes(q)),
    );
  }, [skills, query, scopeFilter]);

  const current =
    (editor?.mode === "edit" && editor.skill) ||
    skills.find((s) => s.location === selected) ||
    filtered[0] ||
    null;

  const confirmDelete = async () => {
    if (!deleting || deletingBusy) return;
    setDeletingBusy(true);
    setDeletingError("");
    try {
      await useApp.getState().deleteSkill(deleting.location);
      if (selected === deleting.location) setSelected(null);
      setDeleting(null);
    } catch (e) {
      setDeletingError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeletingBusy(false);
    }
  };

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-background">
      {/* 顶栏 */}
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border/40 px-4 py-3">
        <Sparkles className="size-4.5 shrink-0 text-primary" />
        <h2 className="mr-2 text-[15px] font-semibold">技能管理</h2>

        {/* 搜索 */}
        <div className="relative ml-auto min-w-40">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground/50" />
          <input
            value={query}
            placeholder="搜索技能"
            className="w-full rounded-lg border border-border bg-card py-1.5 pr-7 pl-8 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30"
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button
              type="button"
              className="absolute top-1/2 right-1.5 -translate-y-1/2 cursor-pointer text-muted-foreground/50 transition-colors hover:text-foreground"
              onClick={() => setQuery("")}
            >
              <X className="size-3.5" />
            </button>
          )}
        </div>

        {/* 作用域过滤 */}
        <div className="flex overflow-hidden rounded-lg border border-border text-xs">
          {(
            [
              ["all", "全部"],
              ["global", "全局"],
              ["project", "项目"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={value}
              type="button"
              className={cn(
                "cursor-pointer px-2.5 py-1.5 transition-colors",
                scopeFilter === value
                  ? "bg-primary/10 font-medium text-primary"
                  : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              )}
              onClick={() => setScopeFilter(value)}
            >
              {label}
            </button>
          ))}
        </div>

        <button
          type="button"
          title="刷新"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={() => reloadSkills()}
        >
          <RefreshCw className="size-4" />
        </button>
        <button
          type="button"
          className="flex cursor-pointer items-center gap-1.5 rounded-lg bg-primary px-2.5 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
          onClick={() => setEditor({ mode: "create" })}
        >
          <Plus className="size-3.5" />
          新建
        </button>
        <button
          type="button"
          title="返回对话 (Esc)"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={close}
        >
          <X className="size-4" />
        </button>
      </div>

      {/* 双栏主体 */}
      <div className="flex min-h-0 flex-1">
        {/* 左：技能列表 */}
        <div className="hlx-input-scroll w-64 shrink-0 space-y-0.5 overflow-y-auto border-r border-border/40 p-2">
          {skillsError ? (
            <div className="flex flex-col items-center gap-2.5 px-3 py-10 text-center">
              <p className="text-xs leading-relaxed text-destructive">
                {skillsError}
              </p>
              <button
                type="button"
                className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                onClick={() => reloadSkills()}
              >
                重试
              </button>
            </div>
          ) : filtered.length === 0 ? (
            <div className="flex flex-col items-center justify-center gap-2 px-3 py-10 text-center">
              <Sparkles className="size-7 text-muted-foreground/40" />
              <p className="text-xs text-muted-foreground/60">
                {skills.length === 0
                  ? "暂无技能，点击右上角「新建」创建"
                  : "无匹配结果"}
              </p>
            </div>
          ) : (
            filtered.map((skill) => (
              <button
                key={skill.location}
                type="button"
                className={cn(
                  "w-full cursor-pointer rounded-lg px-2.5 py-2 text-left transition-colors",
                  current?.location === skill.location
                    ? "bg-primary/[0.08]"
                    : "hover:bg-muted/60",
                )}
                onClick={() => {
                  setSelected(skill.location);
                  setEditor(null);
                }}
                title={skill.description}
              >
                <div className="flex items-center gap-1.5">
                  <span
                    className={cn(
                      "min-w-0 flex-1 truncate font-mono text-[13px]",
                      current?.location === skill.location
                        ? "font-medium text-foreground"
                        : "text-foreground/75",
                    )}
                  >
                    {skill.name}
                  </span>
                  <ScopeBadge scope={skill.scope} />
                </div>
                {skill.description && (
                  <p className="mt-0.5 truncate text-xs text-muted-foreground/70">
                    {skill.description}
                  </p>
                )}
              </button>
            ))
          )}
        </div>

        {/* 右：详情 / 编辑器 */}
        <div className="min-w-0 flex-1 p-5">
          {editor ? (
            <SkillForm
              editor={editor}
              onDone={(name) => {
                setEditor(null);
                setPendingSelect(name);
              }}
              onCancel={() => setEditor(null)}
            />
          ) : current ? (
            <SkillDetail
              skill={current}
              onEdit={() => setEditor({ mode: "edit", skill: current })}
              onDelete={() => {
                setDeletingError("");
                setDeleting(current);
              }}
            />
          ) : (
            <div className="flex h-full flex-col items-center justify-center gap-3 py-12 text-center">
              <Sparkles className="size-10 text-muted-foreground/30" />
              <p className="text-sm text-muted-foreground/70">
                {skillsError ? "加载失败" : "选择左侧技能查看详情"}
              </p>
              {!skillsError && (
                <p className="max-w-sm text-xs leading-relaxed text-muted-foreground/50">
                  技能从全局目录 ~/.hailux/skills/ 与项目目录
                  ./.hailux/skills/ 自动发现；同名时项目级覆盖全局
                </p>
              )}
            </div>
          )}
        </div>
      </div>

      {/* 删除二次确认 */}
      {deleting && (
        <Overlay onClose={() => !deletingBusy && setDeleting(null)}>
          <div className="mb-3 flex items-center gap-2.5">
            <span className="flex size-9 items-center justify-center rounded-lg bg-destructive/10 ring-1 ring-destructive/30">
              <Trash2 className="size-4.5 text-destructive" />
            </span>
            <h3 className="text-[15px] font-semibold">删除技能</h3>
          </div>
          <p className="mb-1 text-sm leading-relaxed text-muted-foreground">
            确定删除技能「{deleting.name}」吗？其所在目录（含
            scripts/ 等附属文件）将被整体删除，不可恢复。
          </p>
          <p className="mb-4 truncate font-mono text-xs text-muted-foreground/50">
            {deleting.location}
          </p>
          {deletingError && (
            <p className="mb-3 text-xs text-destructive">{deletingError}</p>
          )}
          <div className="flex justify-end gap-2">
            <button
              type="button"
              className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              onClick={() => setDeleting(null)}
              disabled={deletingBusy}
            >
              取消
            </button>
            <button
              type="button"
              className="flex cursor-pointer items-center gap-1.5 rounded-lg bg-destructive px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-destructive/90 disabled:cursor-not-allowed disabled:opacity-30"
              onClick={confirmDelete}
              disabled={deletingBusy}
            >
              {deletingBusy && <Loader2 className="size-3.5 animate-spin" />}
              删除
            </button>
          </div>
        </Overlay>
      )}
    </div>
  );
}
