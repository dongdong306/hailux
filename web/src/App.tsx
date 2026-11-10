// 根组件：wendao ChatPage 同构布局 —— 可折叠侧边栏 + 顶部工具栏 + 对话区
import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Database,
  PanelLeft,
  PanelLeftClose,
} from "lucide-react";
import { anyDialogOpen, useApp } from "./store/app-store";
import { HailuxRuntimeProvider } from "./runtime/hailux-runtime";
import { Sidebar } from "./components/sidebar";
import { Thread } from "./components/assistant-ui/thread";
import { ChatInput } from "./components/chat-input";
import {
  AskUserDialog,
  PermissionDialog,
  WorkdirPicker,
} from "./components/dialogs";
import { AddModelDialog, SettingsView } from "./components/settings-view";
import { SkillsManager } from "./components/skills-manager";
import { McpManager } from "./components/mcp-manager";
import { StatsView } from "./components/stats-view";
import { cn, fmtTokens } from "./lib/utils";

/** 上下文占用环形指示（16px）：底环 muted + 进度环 accent，中心不留字（数值在旁边） */
function ContextRing({ pct }: { pct: number | null }) {
  const r = 6;
  const c = 2 * Math.PI * r;
  const ratio = pct === null ? 0 : Math.min(pct, 1);
  const color =
    pct === null
      ? "text-muted-foreground/40"
      : ratio >= 0.9
        ? "text-destructive"
        : ratio >= 0.8
          ? "text-warning"
          : "text-meter-cache";
  return (
    <svg
      viewBox="0 0 16 16"
      className={cn("size-4 shrink-0 -rotate-90", color)}
      aria-hidden="true"
    >
      <circle
        cx="8"
        cy="8"
        r={r}
        fill="none"
        strokeWidth="2.5"
        className="stroke-muted-foreground/20"
      />
      <circle
        cx="8"
        cy="8"
        r={r}
        fill="none"
        strokeWidth="2.5"
        strokeLinecap="round"
        stroke="currentColor"
        strokeDasharray={c}
        strokeDashoffset={c * (1 - ratio)}
        style={{ transition: "stroke-dashoffset 0.6s ease" }}
      />
    </svg>
  );
}

export default function App() {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const initWorkDir = useApp((s) => s.initWorkDir);
  const loadModels = useApp((s) => s.loadModels);
  const activeView = useApp((s) => s.activeView);
  const promptTokens = useApp((s) => s.promptTokens);
  const completionTokens = useApp((s) => s.completionTokens);
  const cachedTokens = useApp((s) => s.cachedTokens);
  // 上下文占用（最后请求的输入+输出 ≈ 当前上下文大小；右上角环形指示用）
  const contextPromptTokens = useApp((s) => s.contextPromptTokens);
  const contextCompletionTokens = useApp((s) => s.contextCompletionTokens);
  const contextWindow = useApp((s) => s.contextWindow);
  const contextUsed = contextPromptTokens + contextCompletionTokens;
  const contextPct =
    contextWindow > 0 ? Math.min(contextUsed / contextWindow, 1) : null;
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
        // 弹窗打开时忽略：菜单层级低于弹窗遮罩，避免开出被遮住的隐形菜单
        if (anyDialogOpen()) return;
        const s = useApp.getState();
        s.setModelMenu(!s.showModelMenu);
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

            {/* 右上角：token 用量 + 上下文占用环——本会话累计；图标用计量三色，数值保持灰阶 */}
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
              <span
                className="flex cursor-default items-center"
                title={`上下文占用 ${fmtTokens(contextUsed)}${contextWindow > 0 ? ` / ${fmtTokens(contextWindow)} · ${Math.round((contextPct ?? 0) * 100)}%` : ""}`}
              >
                <ContextRing pct={contextPct} />
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
        <AddModelDialog />
        <WorkdirPicker />
      </div>
    </HailuxRuntimeProvider>
  );
}
