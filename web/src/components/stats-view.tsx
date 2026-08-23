// 用量统计面板：汇总卡片 + 每日条形图 + 按模型聚合 + 最近请求明细。
// 数据来自 GET /api/stats（messages 表中带 usage 的 assistant 行 = 一次 LLM 请求）。
// 配色走计量三色令牌（--meter-in/cache/out），与应用灰阶体系区分数据编码与界面骨架。
import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  BarChart3,
  Database,
  Loader2,
  RefreshCw,
  Repeat,
  X,
} from "lucide-react";
import { useApp } from "../store/app-store";
import { cn, fmtTokens, shortDir } from "../lib/utils";

function fmt(n: number): string {
  return n.toLocaleString();
}

function StatCard({
  icon,
  label,
  value,
  sub,
  tone,
  gauge,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  sub?: string;
  tone: "in" | "cache" | "out" | "default";
  /** 0-100 的比例指示条（缓存卡片 = 命中率） */
  gauge?: number;
}) {
  const textClass =
    tone === "in"
      ? "text-meter-in"
      : tone === "cache"
        ? "text-meter-cache"
        : tone === "out"
          ? "text-meter-out"
          : "text-foreground";
  const tintClass =
    tone === "in"
      ? "hlx-tint-in"
      : tone === "cache"
        ? "hlx-tint-cache"
        : tone === "out"
          ? "hlx-tint-out"
          : "bg-muted";
  return (
    <div className="flex min-w-0 flex-1 items-center gap-3 rounded-xl border border-border/60 bg-card px-4 py-3">
      <div
        className={cn(
          "flex size-9 shrink-0 items-center justify-center rounded-lg",
          tintClass,
        )}
      >
        <span className={textClass}>{icon}</span>
      </div>
      <div className="min-w-0 flex-1">
        <p className="text-xs text-muted-foreground/70">{label}</p>
        <p className={cn("truncate text-lg leading-tight font-semibold tabular-nums", textClass)}>
          {value}
        </p>
        {gauge !== undefined ? (
          <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-meter-cache/15">
            <div
              className="h-full rounded-full bg-meter-cache/70"
              style={{ width: `${Math.min(Math.max(gauge, 0), 100)}%` }}
            />
          </div>
        ) : (
          sub && <p className="text-[11px] text-muted-foreground/60">{sub}</p>
        )}
      </div>
    </div>
  );
}

/** 图表模式：全部(三段堆叠) / 输入(命中+未命中) / 输出 */
type ChartMode = "all" | "in" | "out";

/** 每日条形图，按当前模式选定度量取窗口内最大值定标；tooltip 直接给出缓存命中率 */
function DailyChart({
  daily,
  mode,
}: {
  daily: {
    date: string;
    requests: number;
    prompt_tokens: number;
    cached_tokens: number;
    completion_tokens: number;
  }[];
  mode: ChartMode;
}) {
  if (daily.length === 0) {
    return (
      <p className="px-1 py-6 text-center text-xs text-muted-foreground/60">
        窗口内暂无请求记录
      </p>
    );
  }
  const metric = (d: (typeof daily)[number]) =>
    mode === "out"
      ? d.completion_tokens
      : mode === "in"
        ? d.prompt_tokens
        : d.prompt_tokens + d.completion_tokens;
  const max = Math.max(...daily.map(metric), 1);
  const seg = (v: number) => `${(v / max) * 100}%`;
  return (
    <div className="space-y-1.5">
      {daily.map((d) => {
        const miss = d.prompt_tokens - d.cached_tokens;
        const rate =
          d.prompt_tokens > 0 ? (d.cached_tokens / d.prompt_tokens) * 100 : 0;
        const title = [
          `${d.date} · 请求 ${d.requests} 次`,
          `输入 ${fmtTokens(d.prompt_tokens)} token`,
          `命中 ${fmtTokens(d.cached_tokens)}（命中率 ${rate.toFixed(1)}%）`,
          `未命中 ${fmtTokens(miss)}`,
          `输出 ${fmtTokens(d.completion_tokens)} token`,
        ].join("\n");
        return (
          <div key={d.date} className="flex items-center gap-2.5" title={title}>
            <span className="w-14 shrink-0 text-right font-mono text-[11px] text-muted-foreground/70">
              {d.date.slice(5)}
            </span>
            <div className="flex h-4 min-w-0 flex-1 overflow-hidden rounded-sm bg-muted/40">
              {(mode === "all" || mode === "in") && (
                <>
                  <div
                    className="h-full bg-meter-cache/85"
                    style={{ width: seg(d.cached_tokens) }}
                  />
                  <div className="hlx-meter-miss h-full" style={{ width: seg(miss) }} />
                </>
              )}
              {(mode === "all" || mode === "out") && (
                <div
                  className="h-full bg-meter-out/85"
                  style={{ width: seg(d.completion_tokens) }}
                />
              )}
            </div>
            <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground/70">
              {fmtTokens(metric(d))}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/** 图例小色块 */
function LegendKey({ className, label }: { className: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5 text-[11px] text-muted-foreground/70">
      <span className={cn("size-2 shrink-0 rounded-[2px]", className)} />
      {label}
    </span>
  );
}

export function StatsView() {
  const stats = useApp((s) => s.stats);
  const statsLoading = useApp((s) => s.statsLoading);
  const statsError = useApp((s) => s.statsError);
  const statsFilter = useApp((s) => s.statsFilter);
  const statsDays = useApp((s) => s.statsDays);
  const workdirs = useApp((s) => s.workdirs);
  const loadStats = useApp((s) => s.loadStats);
  const setStatsFilter = useApp((s) => s.setStatsFilter);
  const setStatsDays = useApp((s) => s.setStatsDays);
  const setView = useApp((s) => s.setView);

  const close = () => setView("chat");

  // 图表展示模式与最近请求折叠（本地 UI 状态，不进全局 store）
  const [chartMode, setChartMode] = useState<ChartMode>("all");
  const [showAllRecent, setShowAllRecent] = useState(false);
  const RECENT_CAP = 10;

  // Esc 返回聊天
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const s = stats?.summary;
  const hitRate =
    s && s.prompt_tokens > 0 ? (s.cached_tokens / s.prompt_tokens) * 100 : 0;

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-background">
      {/* 顶栏 */}
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border/40 px-4 py-3">
        <BarChart3 className="size-4.5 shrink-0 text-primary" />
        <h2 className="mr-2 text-[15px] font-semibold">用量统计</h2>

        {/* 项目筛选 */}
        <select
          value={statsFilter}
          className="ml-auto cursor-pointer rounded-lg border border-border bg-card px-2.5 py-1.5 text-xs text-foreground outline-none transition-colors focus:border-primary/30"
          onChange={(e) => setStatsFilter(e.target.value)}
          title="按项目筛选统计范围"
        >
          <option value="">全部项目</option>
          {workdirs.map((d) => (
            <option key={d} value={d}>
              {shortDir(d)}
            </option>
          ))}
        </select>

        {/* 时间窗口 */}
        <div className="flex overflow-hidden rounded-lg border border-border text-xs">
          {(
            [
              [7, "7 天"],
              [30, "30 天"],
              [0, "全部"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={label}
              type="button"
              className={cn(
                "cursor-pointer px-2.5 py-1.5 transition-colors",
                statsDays === value
                  ? "bg-primary/10 font-medium text-primary"
                  : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              )}
              onClick={() => setStatsDays(value)}
            >
              {label}
            </button>
          ))}
        </div>

        <button
          type="button"
          title="刷新"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={() => loadStats()}
        >
          <RefreshCw className={cn("size-4", statsLoading && "animate-spin")} />
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

      {/* 主体 */}
      <div className="hlx-input-scroll min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
        {statsError ? (
          <div className="flex flex-col items-center gap-2.5 py-12 text-center">
            <p className="text-sm text-destructive">{statsError}</p>
            <button
              type="button"
              className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              onClick={() => loadStats()}
            >
              重试
            </button>
          </div>
        ) : !stats ? (
          <div className="flex items-center justify-center gap-2 py-16 text-muted-foreground/60">
            <Loader2 className="size-4 animate-spin" />
            <span className="text-sm">加载中…</span>
          </div>
        ) : (
          <>
            {/* 汇总卡片 */}
            <div className="flex flex-wrap gap-3">
              <StatCard
                icon={<Repeat className="size-4" />}
                label="请求数"
                value={fmt(stats.summary.requests)}
                sub={stats.days > 0 ? `近 ${stats.days} 天` : "全部时间"}
                tone="default"
              />
              <StatCard
                icon={<ArrowUpFromLine className="size-4" />}
                label="输入 token"
                value={fmt(stats.summary.prompt_tokens)}
                sub={`全部历史 ${fmtTokens(stats.total.prompt_tokens)}`}
                tone="in"
              />
              <StatCard
                icon={<Database className="size-4" />}
                label="缓存命中"
                value={fmt(stats.summary.cached_tokens)}
                sub={`命中率 ${hitRate.toFixed(1)}%`}
                gauge={hitRate}
                tone="cache"
              />
              <StatCard
                icon={<ArrowDownToLine className="size-4" />}
                label="输出 token"
                value={fmt(stats.summary.completion_tokens)}
                sub={`全部历史 ${fmtTokens(stats.total.completion_tokens)}`}
                tone="out"
              />
            </div>

            {/* 每日条形图 */}
            <section className="rounded-xl border border-border/60 bg-card p-4">
              <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
                <h3 className="text-sm font-semibold">每日用量</h3>
                {/* 模式切换：全部 / 只看输入 / 只看输出 */}
                <div className="flex overflow-hidden rounded-lg border border-border text-xs">
                  {(
                    [
                      ["all", "全部"],
                      ["in", "输入"],
                      ["out", "输出"],
                    ] as const
                  ).map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      className={cn(
                        "cursor-pointer px-2.5 py-1 transition-colors",
                        chartMode === value
                          ? "bg-primary/10 font-medium text-primary"
                          : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                      )}
                      onClick={() => setChartMode(value)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
              {/* 图例随模式自适应 */}
              <div className="mb-2.5 flex items-center gap-3">
                {chartMode !== "out" && (
                  <>
                    <LegendKey className="bg-meter-cache/85" label="缓存命中" />
                    <LegendKey className="hlx-meter-miss" label="未命中输入" />
                  </>
                )}
                {chartMode !== "in" && <LegendKey className="bg-meter-out/85" label="输出" />}
                <span className="ml-auto text-[11px] text-muted-foreground/50">
                  悬停查看命中率
                </span>
              </div>
              <DailyChart daily={stats.daily} mode={chartMode} />
            </section>

            <div className="grid gap-4 lg:grid-cols-2">
              {/* 按模型 */}
              <section className="rounded-xl border border-border/60 bg-card p-4">
                <h3 className="mb-3 text-sm font-semibold">按模型</h3>
                {stats.by_model.length === 0 ? (
                  <p className="py-4 text-center text-xs text-muted-foreground/60">
                    暂无数据
                  </p>
                ) : (
                  <table className="w-full text-xs">
                    <thead>
                      <tr className="border-b border-border/50 text-left text-muted-foreground/60">
                        <th className="pb-1.5 font-normal">模型</th>
                        <th className="pb-1.5 text-right font-normal">请求</th>
                        <th className="pb-1.5 text-right font-normal">输入</th>
                        <th className="pb-1.5 text-right font-normal">缓存</th>
                        <th className="pb-1.5 text-right font-normal">输出</th>
                      </tr>
                    </thead>
                    <tbody className="tabular-nums">
                      {stats.by_model.map((m) => (
                        <tr key={m.model} className="border-b border-border/30 last:border-0">
                          <td className="py-1.5 font-mono">{m.model}</td>
                          <td className="py-1.5 text-right">{fmt(m.requests)}</td>
                          <td className="py-1.5 text-right text-meter-in">
                            {fmtTokens(m.prompt_tokens)}
                          </td>
                          <td className="py-1.5 text-right text-meter-cache">
                            {fmtTokens(m.cached_tokens)}
                          </td>
                          <td className="py-1.5 text-right text-meter-out">
                            {fmtTokens(m.completion_tokens)}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </section>

              {/* 最近请求（默认收起，上限展示防止页面过长） */}
              <section className="rounded-xl border border-border/60 bg-card p-4">
                <h3 className="mb-3 text-sm font-semibold">
                  最近请求
                  <span className="ml-1.5 text-xs font-normal text-muted-foreground/60">
                    共 {stats.recent.length} 条
                  </span>
                </h3>
                {stats.recent.length === 0 ? (
                  <p className="py-4 text-center text-xs text-muted-foreground/60">
                    暂无数据
                  </p>
                ) : (
                  <>
                    <table className="w-full text-xs">
                      <thead>
                        <tr className="border-b border-border/50 text-left text-muted-foreground/60">
                          <th className="pb-1.5 font-normal">时间</th>
                          <th className="pb-1.5 font-normal">模型</th>
                          <th className="pb-1.5 text-right font-normal">输入</th>
                          <th className="pb-1.5 text-right font-normal">缓存</th>
                          <th className="pb-1.5 text-right font-normal">输出</th>
                        </tr>
                      </thead>
                      <tbody className="tabular-nums">
                        {(showAllRecent
                          ? stats.recent
                          : stats.recent.slice(0, RECENT_CAP)
                        ).map((r) => (
                          <tr
                            key={`${r.created_at}|${r.model}|${r.work_dir}|${r.prompt_tokens}|${r.completion_tokens}`}
                            className="border-b border-border/30 last:border-0"
                          >
                            <td className="py-1.5 font-mono text-muted-foreground/80">
                              {r.created_at.slice(5, 16)}
                            </td>
                            <td className="max-w-32 truncate py-1.5 font-mono" title={r.model}>
                              {r.model}
                            </td>
                            <td className="py-1.5 text-right text-meter-in">
                              {fmtTokens(r.prompt_tokens)}
                            </td>
                            <td className="py-1.5 text-right text-meter-cache">
                              {fmtTokens(r.cached_tokens)}
                            </td>
                            <td className="py-1.5 text-right text-meter-out">
                              {fmtTokens(r.completion_tokens)}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                    {stats.recent.length > RECENT_CAP && (
                      <button
                        type="button"
                        className="mt-2 cursor-pointer text-xs text-muted-foreground/70 transition-colors hover:text-foreground"
                        onClick={() => setShowAllRecent((v) => !v)}
                      >
                        {showAllRecent
                          ? "收起"
                          : `显示全部 ${stats.recent.length} 条`}
                      </button>
                    )}
                  </>
                )}
              </section>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
