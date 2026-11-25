// Root component: wendao ChatPage isomorphic layout — collapsible sidebar +
// top toolbar + conversation area
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

/** Shared color scheme for usage rings and percentages: ≥90% red / ≥80%
 *  yellow / normal meter green */
function ringColor(pct: number | null): string {
  const ratio = pct === null ? 0 : Math.min(pct, 1);
  return pct === null
    ? "text-muted-foreground/40"
    : ratio >= 0.9
      ? "text-destructive"
      : ratio >= 0.8
        ? "text-warning"
        : "text-meter-cache";
}

/** Context usage ring indicator (16px): muted base ring + accent progress
 *  ring, no text in the center (the number sits beside it) */
function ContextRing({ pct }: { pct: number | null }) {
  const r = 6;
  const c = 2 * Math.PI * r;
  const ratio = pct === null ? 0 : Math.min(pct, 1);
  const color = ringColor(pct);
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
  // Context usage (last request input+output ≈ current context size; feeds
  // the top-right ring indicator)
  const contextPromptTokens = useApp((s) => s.contextPromptTokens);
  const contextCompletionTokens = useApp((s) => s.contextCompletionTokens);
  const contextWindow = useApp((s) => s.contextWindow);
  const contextUsed = contextPromptTokens + contextCompletionTokens;
  const contextPct =
    contextWindow > 0 ? Math.min(contextUsed / contextWindow, 1) : null;
  // The question requestId change remounts AskUserDialog via key, leaving no
  // residual internal state
  const askRequestId = useApp((s) => s.askUser?.requestId);
  // Cache hit rate accumulated in this conversation
  const hitRate =
    promptTokens > 0 ? (cachedTokens / promptTokens) * 100 : 0;

  useEffect(() => {
    // Startup: initialize the project dir (last visited > server default dir)
    // and fetch that project's sessions
    initWorkDir().catch(() => {});
    loadModels().catch(() => {});

    // Global shortcuts
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "n") {
        e.preventDefault();
        useApp.getState().newSession();
      }
      if (e.ctrlKey && e.key === "m") {
        e.preventDefault();
        // Ignore while a dialog is open: the menu layer sits below the dialog
        // overlay, avoiding a hidden covered menu
        if (anyDialogOpen()) return;
        const s = useApp.getState();
        s.setModelMenu(!s.showModelMenu);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [initWorkDir, loadModels]);

  useEffect(() => {
    // Reconcile the global mode when the page becomes visible again (other
    // entry points may have changed YOLO/Plan while the tab was suspended).
    // Only handle the "hidden → visible" transition: the initial visible
    // event overlaps with initWorkDir's reconciliation, skip it
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
        {/* Sidebar */}
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

        {/* Main conversation area */}
        <main className="flex min-w-0 flex-1 flex-col">
          {/* Top toolbar */}
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

            {/* Top right: token usage + context usage ring — accumulated this
                session; icons use the three meter colors, numbers stay grey */}
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
                className="flex cursor-default items-center gap-1"
                title={`上下文占用 ${fmtTokens(contextUsed)}${contextWindow > 0 ? ` / ${fmtTokens(contextWindow)} · ${Math.round((contextPct ?? 0) * 100)}%` : ""}`}
              >
                <ContextRing pct={contextPct} />
                {contextPct !== null && (
                  <span className={ringColor(contextPct)}>
                    {Math.round(contextPct * 100)}%
                  </span>
                )}
              </span>
            </span>
          </div>

          {/* Main area: chat / skills manager / MCP manager / usage stats /
              settings (inline switching) */}
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
