// 根组件：wendao ChatPage 同构布局 —— 可折叠侧边栏 + 顶部工具栏 + 对话区
import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Database,
  PanelLeft,
  PanelLeftClose,
} from "lucide-react";
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
import { AddModelDialog, SettingsView } from "./components/settings-view";
import { SkillsManager } from "./components/skills-manager";
import { McpManager } from "./components/mcp-manager";
import { StatsView } from "./components/stats-view";
import { cn, fmtTokens } from "./lib/utils";

export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const initWorkDir = useApp((s) => s.initWorkDir);
  const loadModels = useApp((s) => s.loadModels);
  const activeView = useApp((s) => s.activeView);
  const promptTokens = useApp((s) => s.promptTokens);
  const completionTokens = useApp((s) => s.completionTokens);
  const cachedTokens = useApp((s) => s.cachedTokens);
  // 提问 requestId 变化时通过 key 强制重挂载 AskUserDialog，内部状态零残留
  const askRequestId = useApp((s) => s.askUser?.requestId);
  // 本次对话累计的缓存命中率
  const hitRate =
    promptTokens > 0 ? (cachedTokens / promptTokens) * 100 : 0;

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

  useEffect(() => {
    // 页面重新可见时对账全局模式（防止标签页挂起期间其他入口改过 YOLO/Plan）。
    // 只处理「隐藏 → 可见」转换：初始可见事件与 initWorkDir 的对账重叠，跳过
    let wasHidden = document.visibilityState === "hidden";
    const onVisible = () => {
      const visible = document.visibilityState === "visible";
      if (visible && wasHidden) {
        useApp.getState().syncMode();
      }
      wasHidden = document.visibilityState === "hidden";
    };
    document.addEventListener("visibilitychange", onVisible);
    return () =>
      document.removeEventListener("visibilitychange", onVisible);
  }, []);

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

            {/* token 用量（右上角）——本会话累计；图标用计量三色，数值保持灰阶 */}
            <span className="ml-auto flex items-center gap-2.5 pr-1 text-xs tabular-nums text-muted-foreground/60">
              <span
                className="flex items-center gap-0.5"
                title={`本会话累计输入 ${promptTokens.toLocaleString()} token`}
              >
                <ArrowUpFromLine className="size-3 text-meter-in/80" />
                {fmtTokens(promptTokens)}
              </span>
              <span
                className="flex items-center gap-0.5"
                title={`本会话累计缓存命中 ${cachedTokens.toLocaleString()} token · 输入 ${promptTokens.toLocaleString()} · 命中率 ${hitRate.toFixed(1)}%`}
              >
                <Database className="size-3 text-meter-cache/80" />
                {fmtTokens(cachedTokens)}
                {promptTokens > 0 && (
                  <span className="text-meter-cache/90">({hitRate.toFixed(1)}%)</span>
                )}
              </span>
              <span
                className="flex items-center gap-0.5"
                title={`本会话累计输出 ${completionTokens.toLocaleString()} token`}
              >
                <ArrowDownToLine className="size-3 text-meter-out/80" />
                {fmtTokens(completionTokens)}
              </span>
            </span>
          </div>

          {/* 主区域：聊天 / 技能管理 / MCP 管理 / 用量统计 / 设置（内联切换） */}
          {activeView === "skills" ? (
            <SkillsManager />
          ) : activeView === "mcp" ? (
            <McpManager />
          ) : activeView === "stats" ? (
            <StatsView />
          ) : activeView === "settings" ? (
            <SettingsView />
          ) : (
            <>
              <Thread />
              <ChatInput />
            </>
          )}
        </main>

        <PermissionDialog />
        <AskUserDialog key={askRequestId} />
        <ModelPicker />
        <AddModelDialog />
        <WorkdirPicker />
      </div>
    </HailuxRuntimeProvider>
  );
}
