import { useEffect, useRef, useState } from "react";
import type { ChatItem } from "../store/app-store";

function ToolCallCard({ item }: { item: ChatItem }) {
  const [open, setOpen] = useState(false);
  let args = item.arguments ?? "";
  try {
    args = JSON.stringify(JSON.parse(args), null, 2);
  } catch {
    // 保留原文
  }
  return (
    <div className="my-1 rounded-md border border-[#30363d] bg-[#161b22] text-sm">
      <button
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-white/5"
        onClick={() => setOpen(!open)}
      >
        <span className="text-[#2f81f7]">⚙</span>
        <span className="font-mono text-xs">{item.name}</span>
        {item.subagent && (
          <span className="rounded bg-[#2f81f7]/20 px-1.5 text-xs text-[#2f81f7]">
            {item.subagent}
          </span>
        )}
        <span className="ml-auto text-xs text-[#8b949e]">{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="hlx-code border-t border-[#30363d]">{args}</div>
      )}
    </div>
  );
}

function ToolResultCard({ item }: { item: ChatItem }) {
  const [open, setOpen] = useState(false);
  const preview = (item.result ?? "").split("\n")[0]?.slice(0, 80) ?? "";
  return (
    <div className="my-1 rounded-md border border-[#30363d] bg-[#161b22] text-sm">
      <button
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-white/5"
        onClick={() => setOpen(!open)}
      >
        <span className="text-[#3fb950]">✓</span>
        <span className="font-mono text-xs text-[#8b949e]">{item.name}</span>
        <span className="truncate text-xs text-[#8b949e]">{preview}</span>
        <span className="ml-auto text-xs text-[#8b949e]">{open ? "▾" : "▸"}</span>
      </button>
      {open && <div className="hlx-code border-t border-[#30363d] whitespace-pre-wrap">{item.result}</div>}
    </div>
  );
}

function ReasoningBlock({ item }: { item: ChatItem }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="my-1">
      <button
        className="flex items-center gap-1 text-xs text-[#8b949e] hover:text-[#e6edf3]"
        onClick={() => setOpen(!open)}
      >
        <span>💭 {open ? "收起思考" : "展开思考"}</span>
        {item.kind === "reasoning-streaming" && (
          <span className="animate-pulse text-[#2f81f7]">·</span>
        )}
      </button>
      {open && (
        <div className="mt-1 whitespace-pre-wrap border-l-2 border-[#30363d] pl-3 text-sm text-[#8b949e]">
          {item.text}
        </div>
      )}
    </div>
  );
}

export function ChatThread({ items }: { items: ChatItem[] }) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);

  useEffect(() => {
    if (autoScrollRef.current) {
      bottomRef.current?.scrollIntoView();
    }
  }, [items]);

  const onScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    autoScrollRef.current = atBottom;
  };

  return (
    <div
      ref={containerRef}
      onScroll={onScroll}
      className="flex-1 overflow-y-auto px-4 py-3"
    >
      {items.map((item, i) => {
        switch (item.kind) {
          case "user":
            return (
              <div key={i} className="my-3 flex justify-end">
                <div className="max-w-[85%] whitespace-pre-wrap rounded-lg bg-[#2f81f7] px-3 py-2 text-sm">
                  {item.text}
                </div>
              </div>
            );
          case "assistant":
          case "assistant-streaming":
            return (
              <div key={i} className="my-3 max-w-full whitespace-pre-wrap text-sm leading-relaxed">
                {item.text}
                {item.kind === "assistant-streaming" && (
                  <span className="ml-0.5 inline-block h-4 w-2 animate-pulse bg-[#2f81f7] align-text-bottom" />
                )}
              </div>
            );
          case "reasoning":
          case "reasoning-streaming":
            return <ReasoningBlock key={i} item={item} />;
          case "tool-call":
            return <ToolCallCard key={i} item={item} />;
          case "tool-result":
            return <ToolResultCard key={i} item={item} />;
          case "notice":
            return (
              <div key={i} className="my-2 text-center text-xs text-[#8b949e]">
                {item.text}
              </div>
            );
          case "compact-marker":
            return (
              <div key={i} className="my-2 border-t border-dashed border-[#30363d] pt-2 text-center text-xs text-[#8b949e]">
                ⏳ {item.text}
              </div>
            );
          case "done": {
            const color =
              item.status === "completed"
                ? "text-[#8b949e]"
                : item.status === "interrupted"
                  ? "text-[#d29922]"
                  : "text-[#f85149]";
            return (
              <div key={i} className={`my-2 text-center text-xs ${color}`}>
                {item.model} · {(item.totalMs ?? 0) / 1000}s ·{" "}
                {item.status === "completed"
                  ? "完成"
                  : item.status === "interrupted"
                    ? "已中断"
                    : "错误"}
              </div>
            );
          }
          case "error":
            return (
              <div key={i} className="my-2 rounded-md border border-[#f85149]/40 bg-[#f85149]/10 px-3 py-2 text-sm text-[#f85149]">
                {item.text}
              </div>
            );
        }
      })}
      <div ref={bottomRef} />
    </div>
  );
}
