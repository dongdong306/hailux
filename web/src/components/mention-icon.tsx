import {
  Container,
  Database,
  File,
  FileArchive,
  FileCode,
  FileCog,
  FileImage,
  FileJson,
  FileText,
  Folder,
  GitBranch,
  Settings,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { cn } from "../lib/utils";

const ICON_MUTED = "text-muted-foreground/60";
const ICON_SKY = "text-sky-600 dark:text-sky-400";
const ICON_AMBER = "text-amber-600 dark:text-amber-400";
const ICON_ORANGE = "text-orange-600 dark:text-orange-400";
const ICON_EMERALD = "text-emerald-600 dark:text-emerald-400";
const ICON_VIOLET = "text-violet-600 dark:text-violet-400";
const ICON_CYAN = "text-cyan-600 dark:text-cyan-400";
const ICON_ROSE = "text-rose-600 dark:text-rose-400";

function iconGroup(exts: string[], Icon: LucideIcon, className: string) {
  return exts.map((e) => [e, { Icon, className }] as const);
}

const EXT_ICONS = new Map<string, { Icon: LucideIcon; className: string }>([
  ...iconGroup(["gitignore", "gitattributes", "gitmodules"], GitBranch, ICON_MUTED),
  ...iconGroup(
    ["toml", "yaml", "yml", "ini", "cfg", "conf", "lock", "properties", "editorconfig"],
    FileCog,
    ICON_MUTED,
  ),
  ...iconGroup(["txt", "log"], FileText, ICON_MUTED),
  ...iconGroup(["sh", "bash", "zsh", "fish", "bat", "cmd", "ps1"], Terminal, ICON_MUTED),
  ...iconGroup(["ts", "tsx", "mts", "cts"], FileCode, ICON_SKY),
  ...iconGroup(
    ["c", "h", "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "m", "mm", "r", "pl", "pm", "lua"],
    FileCode,
    ICON_SKY,
  ),
  ...iconGroup(["md", "mdx"], FileText, ICON_SKY),
  ...iconGroup(["java"], FileCode, ICON_ROSE),
  ...iconGroup(["rb", "erb", "scala"], FileCode, ICON_ROSE),
  ...iconGroup(["php", "phtml", "cs"], FileCode, ICON_VIOLET),
  ...iconGroup(["ex", "exs", "hs"], FileCode, ICON_VIOLET),
  ...iconGroup(["js", "jsx", "mjs", "cjs"], FileCode, ICON_AMBER),
  ...iconGroup(["json", "jsonc", "json5", "webmanifest", "ipynb"], FileJson, ICON_AMBER),
  ...iconGroup(["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "jar"], FileArchive, ICON_AMBER),
  ...iconGroup(["rs"], Settings, ICON_ORANGE),
  ...iconGroup(["html", "htm", "vue", "svelte", "astro"], FileCode, ICON_ORANGE),
  ...iconGroup(["swift", "kt", "kts", "zig"], FileCode, ICON_ORANGE),
  ...iconGroup(["py", "pyw", "pyi"], FileCode, ICON_EMERALD),
  ...iconGroup(["jl", "groovy", "gradle"], FileCode, ICON_EMERALD),
  ...iconGroup(["go", "dart"], FileCode, ICON_CYAN),
  ...iconGroup(["sql", "db", "sqlite", "sqlite3"], Database, ICON_CYAN),
  ...iconGroup(["css", "scss", "less", "styl"], FileCode, ICON_VIOLET),
  ...iconGroup(
    ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "avif", "tiff", "heic"],
    FileImage,
    ICON_VIOLET,
  ),
  ...iconGroup(["pdf"], FileText, ICON_ROSE),
]);

const NAME_ICONS: Array<[string, LucideIcon, string]> = [
  ["dockerfile", Container, ICON_SKY],
  ["containerfile", Container, ICON_SKY],
  ["makefile", FileCog, ICON_MUTED],
];

export function mentionIcon(rel: string): { Icon: LucideIcon; className: string } {
  if (rel.endsWith("/")) return { Icon: Folder, className: ICON_MUTED };
  const name = (rel.split("/").pop() ?? "").toLowerCase();
  for (const [n, Icon, className] of NAME_ICONS) {
    if (name === n) return { Icon, className };
  }
  if (name.startsWith(".env")) return { Icon: FileCog, className: ICON_MUTED };
  const ext = name.slice(name.lastIndexOf(".") + 1);
  return EXT_ICONS.get(ext) ?? { Icon: File, className: ICON_MUTED };
}

export function MentionIcon({
  rel,
  mono = false,
  className,
}: {
  rel: string;
  mono?: boolean;
  className?: string;
}) {
  const { Icon, className: color } = mentionIcon(rel);
  return (
    <Icon className={cn("size-3.5 shrink-0", mono ? className : cn(color, className))} />
  );
}
