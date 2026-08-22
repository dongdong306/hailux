// 聊天区右缘轮次导航（对齐 DeepSeek 网页版）：
// 每轮用户提问一个刻度，刻度紧凑等距垂直居中；当前轮刻度最长最亮，
// 相邻刻度逐级收窄；悬停弹出提问缩略列表，弹层垂直对齐悬停刻度并
// 紧贴刻度左侧，点击跳转到对应提问。
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { useApp } from "../../store/app-store";
import { cn } from "../../lib/utils";

/** 用户提问消息的 DOM 标记契约：thread.tsx 设置属性，本组件查询定位。
 *  两处必须经此常量关联，改名失配时导航静默失效 */
export const USER_MSG_ATTR = "data-hlx-user-msg";
const USER_MSG_SELECTOR = `[${USER_MSG_ATTR}]`;

/** 刻度宽度梯度（距高亮中心的轮数 → 像素宽） */
const tickWidth = (distance: number): string =>
  distance === 0 ? "w-7" : distance === 1 ? "w-5" : distance === 2 ? "w-4" : "w-3";

/** 跳转时目标提问距视口顶部的余量（px） */
const JUMP_MARGIN = 16;

interface ThreadNavProps {
  viewportRef: RefObject<HTMLDivElement | null>;
  contentRef: RefObject<HTMLDivElement | null>;
}

export function ThreadNav({ viewportRef, contentRef }: ThreadNavProps) {
  // 全部提问文本（悬停列表数据源，与 DOM 中的 [data-hlx-user-msg] 一一对应）。
  // 注意：选择器必须返回稳定引用（zustand v5 默认 Object.is 比较），
  // 派生数组放进 useMemo，否则每次渲染新数组 → getSnapshot 不稳 → 无限循环
  const items = useApp((s) => s.items);
  const questions = useMemo(
    () => items.filter((i) => i.kind === "user").map((i) => (i.text ?? "").trim()),
    [items],
  );
  const count = questions.length;

  const [visible, setVisible] = useState(false);
  const [active, setActive] = useState(0);
  /** 梯度高亮跟随的轮次（刻度或列表行悬停即时切换） */
  const [hover, setHover] = useState<number | null>(null);
  /** 锚定刻度中心相对轨道盒的纵坐标（直接测量 DOM，不用公式推算） */
  const [anchorY, setAnchorY] = useState(0);
  const [railH, setRailH] = useState(0);
  const [popupH, setPopupH] = useState(0);
  const popupRef = useRef<HTMLDivElement>(null);
  const railBoxRef = useRef<HTMLDivElement>(null);
  const tickRefs = useRef<(HTMLButtonElement | null)[]>([]);
  /** 锚定的刻度序号（弹层位置跟它走；仅 ref，渲染不依赖） */
  const anchorIdxRef = useRef(0);
  /** 点击锁定：true 时高亮固定为所点刻度，不随滚动重算。
   *  到达跳转目标前：scrollTop 只要朝目标靠近就保持锁定（smooth 动画起点
   *  离目标很远，不能用“距目标 >8px”判断，否则动画第一帧就误解锁）；
   *  反向远离目标（用户往回滚）→ 立即解锁。
   *  到达目标后：再次离开目标（用户继续滚动）→ 解锁恢复自动计算 */
  const lockedRef = useRef(false);
  const jumpTargetRef = useRef(0);
  const arrivedRef = useRef(false);
  const lastScrollTopRef = useRef(0);

  /** 元素顶部相对滚动文档顶部的偏移（含已滚出部分） */
  const docOffset = (vp: HTMLDivElement, el: HTMLElement) =>
    el.getBoundingClientRect().top - vp.getBoundingClientRect().top + vp.scrollTop;

  /** 视口所在轮次：默认按“视口底沿”探针（视口内可见的最新一轮）；
   *  滚动接近顶部（距顶 <40px）时高亮首轮（首问后的短回答撑不满
   *  视口、第二问已进入视口的情况） */
  const calcActive = useCallback((vp: HTMLDivElement): number => {
    const els = vp.querySelectorAll<HTMLElement>(USER_MSG_SELECTOR);
    if (els.length === 0) return 0;
    if (vp.scrollTop < 40) return 0;
    const probe = vp.scrollTop + vp.clientHeight - 4;
    let idx = 0;
    els.forEach((el, i) => {
      if (docOffset(vp, el) <= probe) idx = i;
    });
    return idx;
  }, []);

  /** 可滚动性检测（内容增长 / 新消息 / 卡片展开后调用） */
  const measure = useCallback(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    const scrollable = vp.scrollHeight - vp.clientHeight > 40;
    if (!scrollable || count < 2) {
      setVisible(false);
      return;
    }
    setVisible(true);
    if (lockedRef.current) return;
    setActive((prev) => {
      const next = calcActive(vp);
      return prev === next ? prev : next;
    });
  }, [viewportRef, calcActive, count]);

  // 消息流变化后测量（ResizeObserver 兜底流式增量之外的时机，如首帧）。
  // count 收缩（切换会话）时清理悬停/锚定/跳转锁定与刻度 refs，
  // 防止残留旧会话索引（脱离文档的刻度节点会测出垃圾 anchorY）
  useEffect(() => {
    setHover((h) => (h !== null && h >= count ? null : h));
    if (anchorIdxRef.current >= count) anchorIdxRef.current = Math.max(0, count - 1);
    tickRefs.current.length = count;
    lockedRef.current = false;
    measure();
  }, [count, measure]);

  // 内容高度变化（流式输出 / 新消息 / 工具卡展开折叠）统一由 ResizeObserver 捕获
  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    let raf = 0;
    const ro = new ResizeObserver(() => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        measure();
      });
    });
    ro.observe(content);
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [contentRef, measure]);

  // 滚动仅更新活跃轮次（rAF 节流；值不变时 React 自动跳过重渲染）
  useEffect(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        // 点击跳转锁定期间：smooth 滚动途中 scrollTop 逐步逼近目标，保持锁定；
        // 一旦明显偏离目标（用户手动滚动，滚轮一格远超阈值）→ 解锁并恢复自动计算
        if (lockedRef.current) {
          const d = Math.abs(vp.scrollTop - jumpTargetRef.current);
          const prevD = Math.abs(lastScrollTopRef.current - jumpTargetRef.current);
          if (d <= 8) {
            // 到达目标：保持锁定，等待用户后续滚动
            arrivedRef.current = true;
          } else if (arrivedRef.current || d > prevD + 0.5) {
            // 到达后又离开（继续滚动），或未到达就反向远离目标（往回滚）
            lockedRef.current = false;
          }
          lastScrollTopRef.current = vp.scrollTop;
          if (lockedRef.current) return;
        }
        setActive((prev) => {
          const next = calcActive(vp);
          return prev === next ? prev : next;
        });
      });
    };
    vp.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      vp.removeEventListener("scroll", onScroll);
      if (raf) cancelAnimationFrame(raf);
    };
  }, [viewportRef, calcActive]);

  // 轨道盒尺寸变化（首次渲染 / 窗口缩放）→ 更新轨道高度并重测锚定刻度。
  // 依赖 visible：组件隐藏期间 ref 为空，必须等渲染出轨道后再观察
  useEffect(() => {
    if (!visible) return;
    const el = railBoxRef.current;
    if (!el) return;
    const remeasure = () => {
      setRailH(el.clientHeight);
      const btn = tickRefs.current[anchorIdxRef.current];
      if (btn) setAnchorY(btn.offsetTop + btn.offsetHeight / 2);
    };
    remeasure();
    const ro = new ResizeObserver(remeasure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [visible, count]);

  const jump = useCallback(
    (idx: number) => {
      const vp = viewportRef.current;
      if (!vp) return;
      const el = vp.querySelectorAll<HTMLElement>(USER_MSG_SELECTOR)[idx];
      if (!el) return;
      const top = Math.max(0, docOffset(vp, el) - JUMP_MARGIN);
      // 点击锁定：高亮立即切到所点刻度；smooth 滚动途中不重算
      lockedRef.current = true;
      jumpTargetRef.current = top;
      arrivedRef.current = Math.abs(vp.scrollTop - top) <= 8;
      lastScrollTopRef.current = vp.scrollTop;
      setActive(idx);
      vp.scrollTo({ top, behavior: "smooth" });
    },
    [viewportRef],
  );

  // 弹层打开：记录高度（越界钳制用）并把高亮行滚入可见区
  // （手动设置 scrollTop，避免 scrollIntoView 波动聊天主滚动区）
  useLayoutEffect(() => {
    if (hover === null || !popupRef.current) return;
    const popup = popupRef.current;
    setPopupH(popup.clientHeight);
    const row = popup.querySelector<HTMLElement>("[data-current='true']");
    if (row)
      popup.scrollTop = Math.max(
        0,
        row.offsetTop - popup.clientHeight / 2 + row.offsetHeight / 2,
      );
  }, [hover, count]);

  /** 悬停刻度：锚点切到该刻度并实测其纵坐标 */
  const enterTick = (i: number) => {
    setHover(i);
    anchorIdxRef.current = i;
    const rail = railBoxRef.current;
    const btn = tickRefs.current[i];
    if (rail && btn) {
      setAnchorY(btn.offsetTop + btn.offsetHeight / 2);
      setRailH(rail.clientHeight);
    }
  };

  if (!visible || count < 2) return null;

  // 梯度中心：悬停轮次优先，否则当前滚动位置所在轮
  const center = hover ?? active;

  // 弹层垂直位置 = 锚定刻度纵坐标；超出轨道上下界时按弹层半高钳制
  const half = popupH / 2;
  const lo = half + 4;
  const hi = railH - half - 4;
  const popupTop =
    hi < lo ? railH / 2 : Math.min(Math.max(anchorY, lo), hi);

  return (
    // 轨道容器：固定宽度的完整悬停区（刻度↔弹层连续，不断链），
    // 容器本身穿透点击，仅刻度列与弹层可交互
    <div
      ref={railBoxRef}
      className="pointer-events-none absolute inset-y-4 right-3 z-10 hidden w-7 md:block"
      onMouseLeave={() => setHover(null)}
    >
      {/* 刻度列：紧凑等距、整体垂直居中（仅定位入口，不映射文档高度） */}
      <div className="pointer-events-auto flex h-full w-full flex-col items-end justify-center gap-2">
        {Array.from({ length: count }, (_, i) => (
          <button
            key={i}
            type="button"
            aria-label={questions[i]
              ? `跳转到提问 ${i + 1}：${questions[i]!.slice(0, 30)}`
              : `跳转到第 ${i + 1} 轮提问`}
            ref={(el) => {
              tickRefs.current[i] = el;
            }}
            onMouseEnter={() => enterTick(i)}
            onClick={() => jump(i)}
            className="group flex h-1.5 cursor-pointer items-center justify-end"
          >
            <span
              className={cn(
                "h-0.5 rounded-full transition-all duration-150",
                tickWidth(Math.abs(i - center)),
                i === center
                  ? "bg-foreground"
                  : "bg-muted-foreground/40 group-hover:bg-muted-foreground",
              )}
            />
          </button>
        ))}
      </div>

      {/* 悬停弹层：紧贴刻度左侧（right-full 无间隙），垂直中心对齐锚定刻度 */}
      {hover !== null && (
        <div
          className="pointer-events-auto absolute right-full w-72 -translate-y-1/2"
          style={{ top: popupTop }}
        >
          <div
            ref={popupRef}
            className="max-h-[40vh] overflow-y-auto rounded-lg border border-border bg-popover p-1 shadow-lg"
          >
            {questions.map((q, i) => (
              <button
                key={i}
                type="button"
                data-current={i === center}
                onMouseEnter={() => setHover(i)}
                onClick={() => {
                  jump(i);
                  setHover(null);
                }}
                className={cn(
                  "flex w-full cursor-pointer items-center rounded-md px-2 py-1.5 text-left text-xs",
                  i === center
                    ? "bg-muted font-medium text-foreground"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
              >
                <span className="truncate">{q || "（空提问）"}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
