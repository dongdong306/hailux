// Reasoning: assistant-ui official base styling (Reasoning collapsible panel,
// Markdown rendering)
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Brain, ChevronDown } from "lucide-react";
import {
  useAuiState,
  type ReasoningGroupComponent,
} from "@assistant-ui/react";
import { cn } from "../../lib/utils";
import type { AssistantMeta } from "../../runtime/hailux-runtime";

/** Think timer: ticks live while in progress; when done, shows that segment's
 *  own duration (aligned with the TUI: each AgentThinking block has its own
 *  think_ms) */
function ThinkTimer({
  live,
  startIndex,
  endIndex,
}: {
  live: boolean;
  startIndex: number;
  endIndex: number;
}) {
  const allSegments = useAuiState(
    (s) =>
      (s.message.metadata?.custom as AssistantMeta | undefined)?.thinkSegments,
  );
  const segments = useMemo(
    () =>
      (allSegments ?? []).filter(
        (seg) => seg.partIndex >= startIndex && seg.partIndex <= endIndex,
      ),
    [allSegments, startIndex, endIndex],
  );

  const startedAt = segments.find((s) => s.startedAt !== undefined)?.startedAt;
  const totalMs = segments.reduce((sum, s) => sum + (s.ms ?? 0), 0);

  const [now, setNow] = useState(() => Date.now());
  const ticking = live && startedAt !== undefined;
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 100);
    return () => clearInterval(t);
  }, [ticking]);

  if (ticking && startedAt !== undefined) {
    return (
      <span className="tabular-nums">
        思考中 ({((now - startedAt) / 1000).toFixed(1)}s)
      </span>
    );
  }
  if (totalMs > 0) {
    return <span className="tabular-nums">耗时 {(totalMs / 1000).toFixed(1)}s</span>;
  }
  return null;
}

function ReasoningRoot({
  streaming,
  startIndex,
  endIndex,
  children,
}: {
  streaming: boolean;
  startIndex: number;
  endIndex: number;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [userToggled, setUserToggled] = useState(false);
  // Expanded by default while streaming, collapsed once done; after the user
  // toggles manually, respect that manual state
  const effectiveOpen = userToggled ? open : streaming;
  const scrollRef = useRef<HTMLDivElement>(null);

  // While streaming, follow-scroll to the bottom after content updates (the
  // inner max-h container's height does not change, so the outer chat area
  // perceives no size change); stop following once the user scrolls up past
  // the threshold
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el || !streaming) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 48) {
      el.scrollTop = el.scrollHeight;
    }
  });

  return (
    <div className="my-2 w-full rounded-lg border border-border px-3 py-2">
      <button
        type="button"
        onClick={() => {
          setUserToggled(true);
          setOpen(!effectiveOpen);
        }}
        className="text-muted-foreground hover:text-foreground flex w-full items-center gap-2 py-1.5 text-sm transition-[color,scale] active:scale-[0.98]"
      >
        <Brain className="size-4 shrink-0" />
        <span className="min-w-0 truncate leading-none">思维过程</span>
        <span className="text-xs leading-none">
          <ThinkTimer
            live={streaming}
            startIndex={startIndex}
            endIndex={endIndex}
          />
        </span>
        <ChevronDown
          className={cn(
            "ml-auto size-4 shrink-0 transition-transform duration-200",
            effectiveOpen ? "rotate-0" : "-rotate-90",
          )}
        />
      </button>

      {effectiveOpen && (
        <div className="text-muted-foreground relative overflow-hidden text-sm">
          <div
            ref={scrollRef}
            className="max-h-64 overflow-y-auto ps-6 pt-2 pb-2 leading-relaxed text-pretty"
          >
            {children}
          </div>
        </div>
      )}
    </div>
  );
}

export const ReasoningGroup: ReasoningGroupComponent = ({
  children,
  startIndex,
  endIndex,
}) => {
  const streaming = useAuiState((s) => {
    if (s.message.status?.type !== "running") return false;
    for (let i = startIndex; i <= endIndex; i++) {
      if (s.message.parts[i]?.status.type === "running") return true;
    }
    return false;
  });

  return (
    <ReasoningRoot
      streaming={streaming}
      startIndex={startIndex}
      endIndex={endIndex}
    >
      {children}
    </ReasoningRoot>
  );
};
