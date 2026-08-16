// MCP 服务器管理面板：内联双栏视图（左列表 + 右详情/编辑器），替换聊天区渲染。
// 支持新建/编辑/删除/重连状态展示；保存后后台重连并自动注册工具。
import { useEffect, useState } from "react";
import {
  Globe,
  Loader2,
  Pencil,
  Plug,
  Plus,
  RefreshCw,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import {
  useApp,
  type JsonSchemaNode,
  type McpServerEntry,
  type McpServerInput,
} from "../store/app-store";
import { Overlay } from "./dialogs";
import { cn } from "../lib/utils";

type Editor = { mode: "create" } | { mode: "edit"; server: McpServerEntry };

/** 「KEY=VALUE / KEY: VALUE」多行文本 ↔ Record 互转 */
function parseLines(text: string, sep: "=" | ":"): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf(sep);
    if (idx <= 0) continue;
    const k = trimmed.slice(0, idx).trim();
    const v = trimmed.slice(idx + 1).trim();
    if (k) out[k] = v;
  }
  return out;
}

function recordToLines(record: Record<string, string>, sep: "=" | ":"): string {
  return Object.entries(record)
    .map(([k, v]) => `${k}${sep}${v}`)
    .join("\n");
}

function TransportBadge({ transport }: { transport: string }) {
  const isHttp = transport === "http";
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-0.5 rounded-full px-1.5 py-px text-[10px] font-medium",
        isHttp
          ? "bg-primary/10 text-primary/80"
          : "bg-muted text-muted-foreground/70",
      )}
    >
      {isHttp ? (
        <Globe className="size-2.5" />
      ) : (
        <Terminal className="size-2.5" />
      )}
      {isHttp ? "http" : "stdio"}
    </span>
  );
}

/* ── 新建 / 编辑表单 ────────────────────────────────────────── */
function McpForm({
  editor,
  onDone,
  onCancel,
}: {
  editor: Editor;
  onDone: (name: string) => void;
  onCancel: () => void;
}) {
  const createMcpServer = useApp((s) => s.createMcpServer);
  const updateMcpServer = useApp((s) => s.updateMcpServer);
  const isCreate = editor.mode === "create";
  const edit = editor.mode === "edit" ? editor.server : null;
  const [name, setName] = useState(edit?.name ?? "");
  const [transport, setTransport] = useState<"stdio" | "http">(
    edit?.transport === "http" ? "http" : "stdio",
  );
  const [command, setCommand] = useState(edit?.command ?? "");
  const [args, setArgs] = useState((edit?.args ?? []).join("\n"));
  const [env, setEnv] = useState(recordToLines(edit?.env ?? {}, "="));
  const [url, setUrl] = useState(edit?.url ?? "");
  const [headers, setHeaders] = useState(
    recordToLines(edit?.headers ?? {}, ":"),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const submit = async () => {
    if (busy) return;
    if (!name.trim()) {
      setError("请输入服务器名（字母、数字、-、_、.）");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const input: McpServerInput = {
        name: name.trim(),
        transport,
        ...(transport === "stdio"
          ? {
              command: command.trim(),
              args: args
                .split(/\r?\n/)
                .map((l) => l.trim())
                .filter(Boolean),
              env: parseLines(env, "="),
              url: undefined,
              headers: {},
            }
          : {
              command: undefined,
              args: [],
              env: {},
              url: url.trim(),
              headers: parseLines(headers, ":"),
            }),
      };
      if (isCreate) {
        await createMcpServer(input);
      } else {
        // input.name 与原名不同即改名（store 内转换为 new_name）
        await updateMcpServer(edit!.name, input);
      }
      onDone(input.name);
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
          {isCreate ? "新建 MCP 服务器" : "编辑 MCP 服务器"}
        </h3>
      </div>

      <div className="hlx-input-scroll min-h-0 flex-1 space-y-4 overflow-y-auto pr-1">
        <div>
          <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
            名称
          </label>
          <input
            value={name}
            autoFocus
            placeholder="context7"
            className={cn(inputCls, "font-mono")}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
          <p className="mt-1 text-xs text-muted-foreground/60">
            只能包含字母、数字、-、_、.，且以字母或数字开头；工具以
            mcp__名称__工具 格式注册
          </p>
        </div>

        <div>
          <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
            传输方式
          </label>
          <div className="flex gap-2">
            {(
              [
                ["stdio", "stdio（本地子进程）"],
                ["http", "http（远程服务器）"],
              ] as const
            ).map(([value, label]) => (
              <label
                key={value}
                className={cn(
                  "flex flex-1 cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
                  transport === value
                    ? "border-primary/30 bg-primary/[0.06] text-foreground"
                    : "border-border/50 text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
              >
                <input
                  type="radio"
                  name="mcp-transport"
                  className="accent-primary"
                  checked={transport === value}
                  onChange={() => setTransport(value)}
                />
                {label}
              </label>
            ))}
          </div>
        </div>

        {transport === "stdio" ? (
          <>
            <div>
              <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
                命令（command）
              </label>
              <input
                value={command}
                placeholder="npx"
                className={cn(inputCls, "font-mono")}
                onChange={(e) => setCommand(e.target.value)}
              />
            </div>
            <div>
              <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
                参数（args，每行一个）
              </label>
              <textarea
                value={args}
                placeholder={"-y\n@upstash/context7-mcp"}
                className={cn(inputCls, "hlx-input-scroll min-h-20 resize-y font-mono text-[13px]")}
                onChange={(e) => setArgs(e.target.value)}
              />
            </div>
            <div>
              <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
                环境变量（env，每行 KEY=VALUE）
              </label>
              <textarea
                value={env}
                placeholder="API_KEY=your-key"
                className={cn(inputCls, "hlx-input-scroll min-h-16 resize-y font-mono text-[13px]")}
                onChange={(e) => setEnv(e.target.value)}
              />
            </div>
          </>
        ) : (
          <>
            <div>
              <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
                URL
              </label>
              <input
                value={url}
                placeholder="https://example.com/mcp"
                className={cn(inputCls, "font-mono")}
                onChange={(e) => setUrl(e.target.value)}
              />
            </div>
            <div>
              <label className="mb-1.5 block text-xs font-medium text-muted-foreground">
                自定义 Header（每行 Name: Value，常用于鉴权）
              </label>
              <textarea
                value={headers}
                placeholder="Authorization: Bearer your-token"
                className={cn(inputCls, "hlx-input-scroll min-h-16 resize-y font-mono text-[13px]")}
                onChange={(e) => setHeaders(e.target.value)}
              />
            </div>
          </>
        )}
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <p className="mt-3 text-xs text-muted-foreground/50">
        保存后立即写入 ~/.hailux/mcp.toml 并在后台重连，稍候刷新可查看连接状态
      </p>

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

/** schema 节点的类型标记（array/items、enum 等展开为紧凑文本） */
function schemaTypeLabel(node: JsonSchemaNode): string {
  if (node.type) {
    if (node.type === "array" && node.items?.type) {
      return `array&lt;${node.items.type}&gt;`;
    }
    return node.type;
  }
  if (node.properties) return "object";
  if (node.enum) return "enum";
  return "any";
}

/** 参数类型徽章 */
function TypeBadge({ node }: { node: JsonSchemaNode }) {
  return (
    <span className="shrink-0 rounded bg-primary/[0.08] px-1.5 py-px font-mono text-[10px] leading-4 text-primary/90">
      {schemaTypeLabel(node)}
    </span>
  );
}

/** 必填徽章 */
function RequiredBadge() {
  return (
    <span className="shrink-0 rounded bg-warning/10 px-1 py-px text-[10px] leading-4 font-medium text-warning">
      必填
    </span>
  );
}

/** 工具参数渲染：解析 JSON Schema 的 properties / required。
 *  三列 grid（名称 | 徽章 | 描述）跨行对齐；名称列 max-content
 *  自动取最长参数名实际宽度（max-w 截断超长名称）。 */
function ToolParams({ schema }: { schema: JsonSchemaNode }) {
  const props = schema.properties ?? {};
  const required = new Set(schema.required ?? []);
  const entries = Object.entries(props);

  if (entries.length === 0) return null;

  return (
    <div className="mt-2 rounded-md border border-border/40 bg-muted/40 p-2">
      <p className="mb-1 text-[10px] font-medium tracking-wide text-muted-foreground/50">
        参数
      </p>
      <div className="grid grid-cols-[max-content_max-content_1fr] items-baseline gap-x-2 gap-y-1 text-xs leading-relaxed">
        {entries.flatMap(([pname, pnode]) => [
          <span
            key={pname}
            className="max-w-64 overflow-hidden text-ellipsis whitespace-nowrap font-mono font-medium text-foreground/85"
            title={pname}
          >
            {pname}
          </span>,
          <span key={`${pname}__badges`} className="flex items-center gap-1">
            <TypeBadge node={pnode} />
            {required.has(pname) && <RequiredBadge />}
          </span>,
          <span
            key={`${pname}__desc`}
            className="min-w-0 text-muted-foreground/75"
          >
            {pnode.description ?? ""}
          </span>,
        ])}
      </div>
    </div>
  );
}

/* ── 服务器详情 ─────────────────────────────────────────────── */
function McpDetail({
  server,
  onEdit,
  onDelete,
}: {
  server: McpServerEntry;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="flex h-full flex-col">
      <div className="mb-1 flex items-start gap-2.5">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate font-mono text-base font-semibold text-foreground">
              {server.name}
            </h3>
            <TransportBadge transport={server.transport} />
          </div>
          <p className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
            <span
              className={cn(
                "inline-block size-2 rounded-full",
                server.connected ? "bg-emerald-500/80" : "bg-destructive/60",
              )}
            />
            {server.connected
              ? `已连接 · ${server.tools} 个工具`
              : (server.error ?? "未连接")}
          </p>
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

      {/* 配置概览 */}
      <div className="hlx-code my-3 text-xs leading-relaxed">
        {server.transport === "stdio" ? (
          <>
            <div>
              command: <span className="text-foreground/80">{server.command}</span>
            </div>
            {server.args.length > 0 && (
              <div>
                args: <span className="text-foreground/80">{server.args.join(" ")}</span>
              </div>
            )}
            {Object.keys(server.env).length > 0 && (
              <div className="text-muted-foreground/60">
                env: {Object.keys(server.env).join(", ")}
              </div>
            )}
          </>
        ) : (
          <>
            <div>
              url: <span className="text-foreground/80">{server.url}</span>
            </div>
            {Object.keys(server.headers).length > 0 && (
              <div className="text-muted-foreground/60">
                headers: {Object.keys(server.headers).join(", ")}
              </div>
            )}
          </>
        )}
      </div>

      {/* 工具列表：卡片式，名称 + 描述 + 参数分区 */}
      {server.tool_details.length > 0 ? (
        <>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">
            工具（{server.tool_details.length}）
          </p>
          <div className="hlx-input-scroll min-h-0 flex-1 space-y-2 overflow-y-auto rounded-lg border border-border/50 p-2">
            {server.tool_details.map((t) => (
              <div
                key={t.name}
                className="rounded-lg border border-border/50 bg-card px-3 py-2.5 transition-colors hover:border-border"
              >
                <div className="font-mono text-xs font-semibold text-foreground">
                  {t.name}
                </div>
                {t.description && (
                  <p className="mt-1 text-xs leading-relaxed text-muted-foreground/80">
                    {t.description}
                  </p>
                )}
                {t.schema && <ToolParams schema={t.schema} />}
              </div>
            ))}
          </div>
        </>
      ) : (
        !server.connected && (
          <p className="mt-2 text-xs leading-relaxed text-muted-foreground/60">
            服务器未连接。若刚保存请稍候刷新；持续失败请检查配置（command
            是否在 PATH、URL 是否可达等）。
          </p>
        )
      )}
    </div>
  );
}

/* ── 主面板 ─────────────────────────────────────────────────── */
export function McpManager() {
  const servers = useApp((s) => s.mcpServers);
  const mcpError = useApp((s) => s.mcpError);
  const reloadMcp = useApp((s) => s.reloadMcp);
  const setView = useApp((s) => s.setView);
  const [selected, setSelected] = useState<string | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [deleting, setDeleting] = useState<McpServerEntry | null>(null);
  const [deletingBusy, setDeletingBusy] = useState(false);
  const [deletingError, setDeletingError] = useState("");
  const [pendingSelect, setPendingSelect] = useState<string | null>(null);

  const close = () => {
    setView("chat");
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

  // 保存后按名称选中目标服务器
  useEffect(() => {
    if (!pendingSelect) return;
    const hit = servers.find((s) => s.name === pendingSelect);
    if (hit) {
      setSelected(hit.name);
      setPendingSelect(null);
    }
  }, [servers, pendingSelect]);

  const current =
    (editor?.mode === "edit" && editor.server) ||
    servers.find((s) => s.name === selected) ||
    servers[0] ||
    null;

  const confirmDelete = async () => {
    if (!deleting || deletingBusy) return;
    setDeletingBusy(true);
    setDeletingError("");
    try {
      await useApp.getState().deleteMcpServer(deleting.name);
      if (selected === deleting.name) setSelected(null);
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
        <Plug className="size-4.5 shrink-0 text-primary" />
        <h2 className="mr-2 text-[15px] font-semibold">MCP 服务器</h2>

        <div className="ml-auto" />
        <button
          type="button"
          title="刷新"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={() => reloadMcp()}
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
        {/* 左：服务器列表 */}
        <div className="hlx-input-scroll w-64 shrink-0 space-y-0.5 overflow-y-auto border-r border-border/40 p-2">
          {mcpError ? (
            <div className="flex flex-col items-center gap-2.5 px-3 py-10 text-center">
              <p className="text-xs leading-relaxed text-destructive">
                {mcpError}
              </p>
              <button
                type="button"
                className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                onClick={() => reloadMcp()}
              >
                重试
              </button>
            </div>
          ) : servers.length === 0 ? (
            <div className="flex flex-col items-center justify-center gap-2 px-3 py-10 text-center">
              <Plug className="size-7 text-muted-foreground/40" />
              <p className="text-xs text-muted-foreground/60">
                暂无服务器，点击右上角「新建」添加
              </p>
            </div>
          ) : (
            servers.map((server) => (
              <button
                key={server.name}
                type="button"
                className={cn(
                  "w-full cursor-pointer rounded-lg px-2.5 py-2 text-left transition-colors",
                  current?.name === server.name
                    ? "bg-primary/[0.08]"
                    : "hover:bg-muted/60",
                )}
                onClick={() => {
                  setSelected(server.name);
                  setEditor(null);
                }}
              >
                <div className="flex items-center gap-1.5">
                  <span
                    className={cn(
                      "inline-block size-2 shrink-0 rounded-full",
                      server.connected
                        ? "bg-emerald-500/80"
                        : "bg-destructive/60",
                    )}
                    title={server.connected ? "已连接" : "未连接"}
                  />
                  <span
                    className={cn(
                      "min-w-0 flex-1 truncate font-mono text-[13px]",
                      current?.name === server.name
                        ? "font-medium text-foreground"
                        : "text-foreground/75",
                    )}
                  >
                    {server.name}
                  </span>
                  <TransportBadge transport={server.transport} />
                </div>
                <p className="mt-0.5 truncate pl-3.5 text-xs text-muted-foreground/70">
                  {server.connected
                    ? `${server.tools} 工具`
                    : (server.error ?? "未连接")}
                </p>
              </button>
            ))
          )}
        </div>

        {/* 右：详情 / 编辑器 */}
        <div className="min-w-0 flex-1 p-5">
          {editor ? (
            <McpForm
              editor={editor}
              onDone={(name) => {
                setEditor(null);
                setPendingSelect(name);
              }}
              onCancel={() => setEditor(null)}
            />
          ) : current ? (
            <McpDetail
              server={current}
              onEdit={() => setEditor({ mode: "edit", server: current })}
              onDelete={() => {
                setDeletingError("");
                setDeleting(current);
              }}
            />
          ) : (
            <div className="flex h-full flex-col items-center justify-center gap-3 py-12 text-center">
              <Plug className="size-10 text-muted-foreground/30" />
              <p className="text-sm text-muted-foreground/70">
                {mcpError ? "加载失败" : "选择左侧服务器查看详情"}
              </p>
              {!mcpError && (
                <p className="max-w-sm text-xs leading-relaxed text-muted-foreground/50">
                  MCP 服务器配置于 ~/.hailux/mcp.toml，支持 stdio（本地子进程）
                  与 http（远程）两种传输；连接成功后工具自动注册给模型调用
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
            <h3 className="text-[15px] font-semibold">删除 MCP 服务器</h3>
          </div>
          <p className="mb-4 text-sm leading-relaxed text-muted-foreground">
            确定删除「{deleting.name}」吗？将从 ~/.hailux/mcp.toml
            移除该配置并断开连接。
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
