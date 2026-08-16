// 侧边栏：项目选择（顶部）+ 对话列表（先选项目，再选对话）+ 底部技能/MCP 导航
import { useState } from "react";
import {
  ChevronsUpDown,
  Folder,
  MessageSquare,
  Plug,
  Plus,
  Sparkles,
  Trash2,
} from "lucide-react";
import { useApp } from "../store/app-store";
import { Overlay } from "./dialogs";
import { cn, relativeTime, shortDir } from "../lib/utils";

function SessionItem({
  id,
  title,
  updatedAt,
  active,
  disabled,
}: {
  id: string;
  title: string;
  updatedAt: string;
  active: boolean;
  disabled: boolean;
}) {
  const switchSession = useApp((s) => s.switchSession);
  const deleteSession = useApp((s) => s.deleteSession);
  const [confirming, setConfirming] = useState(false);

  return (
    <div
      className={cn(
        "group relative flex items-center rounded-lg transition-all duration-150",
        active ? "bg-primary/[0.08]" : "hover:bg-muted/60",
      )}
    >
      <button
        type="button"
        disabled={disabled}
        onClick={() => switchSession(id)}
        title={updatedAt ? `${title || "新对话"} · ${relativeTime(updatedAt)}` : title}
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5 px-3 py-2.5 text-left focus-visible:rounded-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50"
      >
        <MessageSquare
          className={cn(
            "size-3.5 shrink-0 transition-colors",
            active ? "text-primary" : "text-muted-foreground/60",
          )}
        />
        <span
          className={cn(
            "truncate text-sm transition-colors",
            active ? "font-medium text-foreground" : "text-foreground/70",
          )}
        >
          {title || "新对话"}
        </span>
      </button>

      {/* 删除按钮（hover 显示；点击弹二次确认） */}
      <button
        type="button"
        title="删除对话"
        disabled={disabled}
        className="mr-1.5 flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-md p-1.5 text-muted-foreground/40 transition-all opacity-0 group-hover:opacity-100 hover:bg-destructive/10 hover:text-destructive focus:opacity-100 disabled:cursor-not-allowed"
        onClick={() => setConfirming(true)}
      >
        <Trash2 className="size-3" />
      </button>

      {/* 删除二次确认弹窗 */}
      {confirming && (
        <Overlay onClose={() => setConfirming(false)}>
          <div className="mb-3 flex items-center gap-2.5">
            <span className="flex size-9 items-center justify-center rounded-lg bg-destructive/10 ring-1 ring-destructive/30">
              <Trash2 className="size-4.5 text-destructive" />
            </span>
            <h3 className="text-[15px] font-semibold">删除对话</h3>
          </div>
          <p className="mb-4 text-sm leading-relaxed text-muted-foreground">
            确定删除「{title || "新对话"}」吗？删除后不可恢复。
          </p>
          <div className="flex justify-end gap-2">
            <button
              type="button"
              className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              onClick={() => setConfirming(false)}
            >
              取消
            </button>
            <button
              type="button"
              className="cursor-pointer rounded-lg bg-destructive px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-destructive/90"
              onClick={() => deleteSession(id)}
            >
              删除
            </button>
          </div>
        </Overlay>
      )}
    </div>
  );
}

export function Sidebar() {
  const sessions = useApp((s) => s.sessions);
  const sessionId = useApp((s) => s.sessionId);
  const workDir = useApp((s) => s.workDir);
  const isRunning = useApp((s) => s.isRunning);
  const activeView = useApp((s) => s.activeView);
  const newSession = useApp((s) => s.newSession);
  const setWorkdirPicker = useApp((s) => s.setWorkdirPicker);
  const setView = useApp((s) => s.setView);

  return (
    <div className="flex h-full flex-col">
      {/* 顶部：项目标题 + 当前项目条（点击切换项目） */}
      <div className="shrink-0 px-4 pt-3.5">
        <h2 className="text-base font-semibold tracking-tight text-foreground">
          项目
        </h2>
      </div>
      <div className="shrink-0 px-3 pt-2 pb-2">
        <button
          type="button"
          onClick={() => setWorkdirPicker(true)}
          title={workDir || "选择项目目录"}
          className="flex w-full cursor-pointer items-center gap-2.5 rounded-lg border border-primary/25 bg-primary/[0.06] px-3 py-2.5 text-left transition-colors hover:border-primary/40 hover:bg-primary/10"
        >
          <Folder className="size-4.5 shrink-0 text-primary" />
          <span className="min-w-0 flex-1 truncate text-[15px] font-medium text-foreground">
            {workDir ? shortDir(workDir) : "未选择项目"}
          </span>
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground/60" />
        </button>
      </div>

      {/* 对话 + 新建（当前项目内） */}
      <div className="flex shrink-0 items-center justify-between px-4 py-2">
        <h2 className="text-sm font-semibold tracking-tight text-foreground/80">
          对话
        </h2>
        <button
          type="button"
          onClick={() => newSession()}
          disabled={isRunning}
          aria-label="新建对话"
          title="在当前项目新建会话（Ctrl+N）"
          className="flex size-7 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40"
        >
          <Plus className="size-4" />
        </button>
      </div>

      {/* 会话列表：当前项目平铺 */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {sessions.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <MessageSquare className="size-8 text-muted-foreground/40" />
            <p className="mt-2 text-xs text-muted-foreground/60">
              {workDir ? "此项目暂无对话" : "请先选择项目"}
            </p>
          </div>
        ) : (
          <div className="space-y-0.5">
            {sessions.map((session) => (
              <SessionItem
                key={session.id}
                id={session.id}
                title={session.title}
                updatedAt={session.updated_at}
                active={session.id === sessionId}
                disabled={isRunning}
              />
            ))}
          </div>
        )}
      </div>

      {/* 底部导航：切换主区域视图，再次点击已激活项回到对话 */}
      <div className="shrink-0 space-y-0.5 border-t border-border/40 p-2">
        <button
          type="button"
          onClick={() => setView(activeView === "skills" ? "chat" : "skills")}
          className={cn(
            "flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors",
            activeView === "skills"
              ? "bg-primary/[0.08] font-medium text-foreground"
              : "text-foreground/70 hover:bg-muted/60 hover:text-foreground",
          )}
        >
          <Sparkles
            className={cn(
              "size-3.5 shrink-0",
              activeView === "skills"
                ? "text-primary"
                : "text-muted-foreground/70",
            )}
          />
          技能管理
        </button>
        <button
          type="button"
          onClick={() => setView(activeView === "mcp" ? "chat" : "mcp")}
          className={cn(
            "flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors",
            activeView === "mcp"
              ? "bg-primary/[0.08] font-medium text-foreground"
              : "text-foreground/70 hover:bg-muted/60 hover:text-foreground",
          )}
        >
          <Plug
            className={cn(
              "size-3.5 shrink-0",
              activeView === "mcp"
                ? "text-primary"
                : "text-muted-foreground/70",
            )}
          />
          MCP 服务器
        </button>
      </div>
    </div>
  );
}
