// Turn navigation on the chat area's right edge (aligned with the DeepSeek
// web UI): one tick per user question; ticks are compact, evenly spaced, and
// vertically centered; the active tick is the longest and brightest, with
// adjacent ticks narrowing progressively; hovering pops up a thumbnail list
// of questions, vertically aligned with the hovered tick and hugging its
// left side; clicking jumps to the corresponding question.
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

/** DOM marker contract for user question messages: thread.tsx sets the
 *  attribute, this component queries it for positioning.
 *  Both sides must go through this constant; a rename mismatch makes
 *  navigation silently fail */
export const USER_MSG_ATTR = "data-hlx-user-msg";
const USER_MSG_SELECTOR = `[${USER_MSG_ATTR}]`;

/** Tick width gradient (turn distance from the highlighted center → pixel width) */
const tickWidth = (distance: number): string =>
  distance === 0 ? "w-7" : distance === 1 ? "w-5" : distance === 2 ? "w-4" : "w-3";

/** Top-of-viewport margin (px) for the target question when jumping */
const JUMP_MARGIN = 16;

interface ThreadNavProps {
  viewportRef: RefObject<HTMLDivElement | null>;
  contentRef: RefObject<HTMLDivElement | null>;
}

export function ThreadNav({ viewportRef, contentRef }: ThreadNavProps) {
  // All question texts (data source for the hover list, one-to-one with
  // [data-hlx-user-msg] in the DOM).
  // Note: the selector must return a stable reference (zustand v5 compares
  // with Object.is by default); put the derived array in useMemo, otherwise
  // every render makes a new array → unstable getSnapshot → infinite loop
  const items = useApp((s) => s.items);
  const questions = useMemo(
    () => items.filter((i) => i.kind === "user").map((i) => (i.text ?? "").trim()),
    [items],
  );
  const count = questions.length;

  const [visible, setVisible] = useState(false);
  const [active, setActive] = useState(0);
  /** Turn the gradient highlight follows (switched instantly on tick or list-row hover) */
  const [hover, setHover] = useState<number | null>(null);
  /** Y coordinate of the anchor tick's center relative to the rail box
   *  (measured from the DOM directly, not derived by formula) */
  const [anchorY, setAnchorY] = useState(0);
  const [railH, setRailH] = useState(0);
  const [popupH, setPopupH] = useState(0);
  const popupRef = useRef<HTMLDivElement>(null);
  const railBoxRef = useRef<HTMLDivElement>(null);
  const tickRefs = useRef<(HTMLButtonElement | null)[]>([]);
  /** Anchored tick index (popup position follows it; ref only, unused in render) */
  const anchorIdxRef = useRef(0);
  /** Click lock: when true, the highlight stays pinned to the clicked tick
   *  and is not recomputed on scroll.
   *  Before reaching the jump target: keep the lock as long as scrollTop
   *  moves toward the target (a smooth scroll starts far from the target, so
   *  a "distance to target > 8px" test would mis-unlock on the animation's
   *  first frame);
   *  moving away from the target (user scrolls back) → unlock immediately.
   *  After reaching the target: leaving it again (user keeps scrolling) →
   *  unlock and resume auto computation */
  const lockedRef = useRef(false);
  const jumpTargetRef = useRef(0);
  const arrivedRef = useRef(false);
  const lastScrollTopRef = useRef(0);

  /** Element top offset relative to the scrolling document's top (includes
   *  the already-scrolled-out portion) */
  const docOffset = (vp: HTMLDivElement, el: HTMLElement) =>
    el.getBoundingClientRect().top - vp.getBoundingClientRect().top + vp.scrollTop;

  /** Turn the viewport is in: by default uses a "viewport bottom edge" probe
   *  (the latest turn visible inside the viewport);
   *  when scrolled near the top (<40px from the top), highlights the first
   *  turn (covers the case where the short answer after the first question
   *  cannot fill the viewport while the second question is already in view) */
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

  /** Scrollability check (called after content growth / new messages / card expansion) */
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

  // Measure after the message stream changes (ResizeObserver is the fallback
  // for moments outside streaming increments, e.g. the first frame).
  // When count shrinks (session switch), clear hover/anchor/jump lock and the
  // tick refs to prevent stale old-session indices (detached tick nodes would
  // measure a garbage anchorY)
  useEffect(() => {
    setHover((h) => (h !== null && h >= count ? null : h));
    if (anchorIdxRef.current >= count) anchorIdxRef.current = Math.max(0, count - 1);
    tickRefs.current.length = count;
    lockedRef.current = false;
    measure();
  }, [count, measure]);

  // Content height changes (streaming output / new messages / tool card
  // expand-collapse) are all captured by the ResizeObserver
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

  // Scrolling only updates the active turn (rAF-throttled; React skips the
  // re-render automatically when the value is unchanged)
  useEffect(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        // During the click-jump lock: while smooth scrolling, scrollTop
        // approaches the target step by step, keep the lock;
        // once clearly deviating from the target (user manual scroll, one
        // wheel notch far exceeds the threshold) → unlock and resume auto
        // computation
        if (lockedRef.current) {
          const d = Math.abs(vp.scrollTop - jumpTargetRef.current);
          const prevD = Math.abs(lastScrollTopRef.current - jumpTargetRef.current);
          if (d <= 8) {
            // Target reached: keep the lock, wait for the user's next scroll
            arrivedRef.current = true;
          } else if (arrivedRef.current || d > prevD + 0.5) {
            // Left again after arrival (kept scrolling), or moved away from
            // the target before arrival (scrolled back)
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

  // Rail box size changes (first render / window resize) → update the rail
  // height and remeasure the anchored tick.
  // Depends on visible: refs are null while the component is hidden, so wait
  // for the rail to render before observing
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
      // Click lock: the highlight switches to the clicked tick immediately;
      // no recomputation during smooth scrolling
      lockedRef.current = true;
      jumpTargetRef.current = top;
      arrivedRef.current = Math.abs(vp.scrollTop - top) <= 8;
      lastScrollTopRef.current = vp.scrollTop;
      setActive(idx);
      vp.scrollTo({ top, behavior: "smooth" });
    },
    [viewportRef],
  );

  // Popup opened: record its height (used for out-of-bounds clamping) and
  // scroll the highlighted row into view
  // (set scrollTop manually to keep scrollIntoView from jarring the chat's
  // main scroll area)
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

  /** Hovering a tick: switch the anchor to it and measure its Y directly */
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

  // Gradient center: the hovered turn wins, otherwise the turn at the current
  // scroll position
  const center = hover ?? active;

  // Popup vertical position = anchored tick's Y; clamped by half the popup
  // height when it exceeds the rail's top/bottom bounds
  const half = popupH / 2;
  const lo = half + 4;
  const hi = railH - half - 4;
  const popupTop =
    hi < lo ? railH / 2 : Math.min(Math.max(anchorY, lo), hi);

  return (
    // Rail container: fixed-width full hover area (tick↔popup stays
    // continuous, no broken chain);
    // the container itself passes clicks through, only the tick column and
    // popup are interactive
    <div
      ref={railBoxRef}
      className="pointer-events-none absolute inset-y-4 right-3 z-10 hidden w-7 md:block"
      onMouseLeave={() => setHover(null)}
    >
      {/* Tick column: compact, evenly spaced, vertically centered as a whole
          (a positioning entry point only, does not map document height) */}
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

      {/* Hover popup: hugs the tick column's left side (right-full, no gap),
          vertical center aligned with the anchored tick */}
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
