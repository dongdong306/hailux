export interface MentionSegment {
  text: string;
  kind?: string;
  value?: string;
}

function utf16IndexOf(text: string, charOffset: number): number {
  let u16 = 0;
  let seen = 0;
  for (const ch of text) {
    if (seen === charOffset) return u16;
    u16 += ch.length;
    seen += 1;
  }
  return u16;
}

export function splitByMentions(
  text: string,
  mentions: Array<{ start: number; end: number; kind?: string }> | undefined,
): MentionSegment[] {
  const hits: Array<{ start: number; end: number; kind: string; value: string }> = [];
  for (const m of mentions ?? []) {
    const start = utf16IndexOf(text, m.start);
    const end = utf16IndexOf(text, m.end);
    if (end > start) {
      hits.push({ start, end, kind: m.kind ?? "file", value: text.slice(start, end) });
    }
  }
  hits.sort((a, b) => a.start - b.start);

  const segments: MentionSegment[] = [];
  let last = 0;
  for (const h of hits) {
    if (h.start > last) {
      segments.push({ text: text.slice(last, h.start) });
    }
    segments.push({ text: text.slice(h.start, h.end), kind: h.kind, value: h.value });
    last = h.end;
  }
  if (last < text.length) {
    segments.push({ text: text.slice(last) });
  }
  return segments;
}

export function mentionDisplayPath(token: string, workDir: string): string {
  if (!workDir) return token;
  const wd = workDir.replace(/[\\/]+$/, "").replace(/\\/g, "/").toLowerCase();
  if (!wd) return token;
  const p = token.replace(/\\/g, "/");
  if (!p.toLowerCase().startsWith(`${wd}/`)) return token;
  return p.slice(wd.length + 1);
}
