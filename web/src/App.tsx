// 根组件：wendao ChatPage 同构布局 —— 可折叠侧边栏 + 顶部工具栏 + 对话区
import { useEffect, useState } from "react";
import { ArrowDownToLine, ArrowUpFromLine, PanelLeft, PanelLeftClose } from "lucide-react";
import { useApp } from "./store/app-store";
import { HailuxRuntimeProvider } from "./runtime/hailux-runtime";
import { Sidebar } from "./components/sidebar";
import { Thread } from "./components/assistant-ui/thread";
import { ChatInput } from "./components/chat-input";
import {
  AskUserDialog,
  ModelPicker,
  PermissionDialog,
  WorkdirPicker,
} from "./components/dialogs";
import { SkillsManager } from "./components/skills-manager";
import { McpManager } from "./components/mcp-manager";
import { cn } from "./lib/utils";

export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const initWorkDir = useApp((s) => s.initWorkDir);
  const loadModels = useApp((s) => s.loadModels);
  const activeView = useApp((s) => s.activeView);
  const promptTokens = useApp((s) => s.promptTokens);
  const completionTokens = useApp((s) => s.completionTokens);

  useEffect(() => {
    // 启动：初始化项目目录（上次访问 > 服务器默认目录）并拉取该项目会话
    initWorkDir().catch(() => {});
    loadModels().catch(() => {});

    // 全局快捷键
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "n") {
        e.preventDefault();
        useApp.getState().newSession();
      }
      if (e.ctrlKey && e.key === "m") {
        e.preventDefault();
        useApp.getState().setModelPicker(true);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [initWorkDir, loadModels]);

  return (
    <HailuxRuntimeProvider>
      <div className="flex h-dvh overflow-hidden bg-background">
        {/* 侧边栏 */}
        <aside
          className={cn(
            "shrink-0 border-r border-border/60 bg-muted/20 transition-all duration-300 ease-out",
            sidebarOpen ? "w-72" : "w-0",
          )}
        >
          <div
            className={cn(
              "h-full w-72 transition-opacity duration-300",
              sidebarOpen ? "opacity-100" : "opacity-0 pointer-events-none",
            )}
          >
            <Sidebar />
          </div>
        </aside>

        {/* 主对话区 */}
        <main className="flex min-w-0 flex-1 flex-col">
          {/* 顶部工具栏 */}
          <div className="flex h-12 shrink-0 items-center border-b border-border/40 px-3">
            <button
              type="button"
              onClick={() => setSidebarOpen(!sidebarOpen)}
              aria-label={sidebarOpen ? "收起侧边栏" : "展开侧边栏"}
              className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            >
              {sidebarOpen ? (
                <PanelLeftClose className="size-4" />
              ) : (
                <PanelLeft className="size-4" />
              )}
            </button>
            <span className="ml-2 text-sm font-medium text-muted-foreground">
              hailux
            </span>

            {/* token 用量（右上角） */}
            <span className="ml-auto flex items-center gap-2.5 pr-1 text-xs tabular-nums text-muted-foreground/60">
              <span className="flex items-center gap-0.5" title="输入 token">
                <ArrowUpFromLine className="size-3" />
                {promptTokens.toLocaleString()}
              </span>
              <span className="flex items-center gap-0.5" title="输出 token">
                <ArrowDownToLine className="size-3" />
                {completionTokens.toLocaleString()}
              </span>
            </span>
          </div>

          {/* 主区域：聊天 / 技能管理 / MCP 管理（内联切换） */}
          {activeView === "skills" ? (
            <SkillsManager />
          ) : activeView === "mcp" ? (
            <McpManager />
          ) : (
            <>
              <Thread />
              <ChatInput />
            </>
          )}
        </main>

        <PermissionDialog />
        <AskUserDialog />
        <ModelPicker />
        <WorkdirPicker />
      </div>
    </HailuxRuntimeProvider>
  );
}
