// Settings panel: inline view replacing the chat area.
// Currently only the "model providers" section: one card per provider (credentials + model list),
// supporting add model / add provider / edit model / edit provider / delete model / delete provider /
// set as default. Further sections can be added here later.
import { useCallback, useEffect, useState } from "react";
import {
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Settings,
  Trash2,
  X,
} from "lucide-react";
import { anyDialogOpen, useApp } from "../store/app-store";
import { cn } from "../lib/utils";
import type { DiscoveredModel, ModelInfo, ProviderOption } from "../runtime/types";
import { Overlay } from "./dialogs";

/* Button styles (same as dialogs.tsx) */
const btnPrimary =
  "cursor-pointer rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30";
const btnOutline =
  "cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-30";

const inputCls =
  "w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30";

/** Default context window (256K) */
const DEFAULT_CONTEXT_WINDOW = "262144";

/* ── Settings panel ───────────────────────────────────────── */
export function SettingsView() {
  const setView = useApp((s) => s.setView);
  const models = useApp((s) => s.models);
  const providers = useApp((s) => s.providers);
  const switchModel = useApp((s) => s.switchModel);
  const setAddModel = useApp((s) => s.setAddModel);
  /** Provider whose fetch-models dialog is open */
  const [fetchTarget, setFetchTarget] = useState<{
    providerId: string;
    providerName: string;
  } | null>(null);
  /** Delete target: a single model / the whole provider */
  const [deleting, setDeleting] = useState<
    | { type: "model"; model: ModelInfo }
    | { type: "provider"; providerId: string; providerName: string }
    | null
  >(null);
  /** Edit target: model parameters / provider credentials */
  const [editingModel, setEditingModel] = useState<ModelInfo | null>(null);
  const [editingProvider, setEditingProvider] = useState<{
    providerId: string;
    providerName: string;
    predefined: boolean;
    baseUrl?: string;
  } | null>(null);

  // Contract: the backend returns models grouped contiguously by provider (BTreeMap order);
  // grouping here relies on adjacency, so non-contiguous ordering would require accumulating by providerId.
  const groups: {
    name: string;
    providerId: string;
    predefined: boolean;
    baseUrl?: string;
    models: ModelInfo[];
  }[] = [];
  for (const m of models) {
    const last = groups[groups.length - 1];
    if (last && last.providerId === m.provider_id) {
      last.models.push(m);
    } else {
      const p = providers.find((x) => x.id === m.provider_id);
      groups.push({
        name: m.provider_name,
        providerId: m.provider_id,
        predefined: m.provider_predefined,
        baseUrl: p?.base_url,
        models: [m],
      });
    }
  }

  // Esc returns to chat
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (anyDialogOpen()) return; // Global dialogs win: Esc closes the dialog only, not the view
      // Local dialogs (delete confirm / edit / fetch models) behave the same: Esc closes the dialog, not the view
      if (deleting || editingModel || editingProvider || fetchTarget) return;
      setView("chat");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setView, deleting, editingModel, editingProvider, fetchTarget]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Top bar */}
      <div className="flex shrink-0 items-center gap-2 border-b border-border/40 px-4 py-3">
        <Settings className="size-4.5 shrink-0 text-primary" />
        <h2 className="mr-2 text-[15px] font-semibold">设置</h2>
        <div className="ml-auto" />
        <button
          type="button"
          title="返回对话 (Esc)"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={() => setView("chat")}
        >
          <X className="size-4" />
        </button>
      </div>

      {/* Content area */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-3xl px-4 py-6">
          {/* ── Model providers section ── */}
          <div className="mb-3 flex items-center justify-between">
            <div>
              <h3 className="text-[15px] font-semibold text-foreground">模型提供商</h3>
              <p className="mt-0.5 text-xs text-muted-foreground/60">
                管理 API 服务商与模型：可编辑模型参数、更换凭据与端点；内置服务商本身不可删除或改名
              </p>
            </div>
            <button
              type="button"
              className={cn(btnOutline, "flex items-center gap-1.5")}
              onClick={() => setAddModel(true)}
            >
              <Plus className="size-3.5" />
              添加模型 / 服务商
            </button>
          </div>

          <div className="space-y-3">
            {groups.length === 0 && (
              <p className="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground/60">
                暂无已配置的服务商，点击「添加模型 / 服务商」开始
              </p>
            )}
            {groups.map((g) => (
              <div
                key={g.providerId}
                className="rounded-xl border border-border/60 bg-card/50 p-4"
              >
                {/* Card header: provider info + actions */}
                <div className="mb-3 flex items-start gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium text-foreground">{g.name}</span>
                      {!g.predefined && (
                        <span className="rounded-full bg-primary/10 px-1.5 py-px text-[10px] font-medium text-primary/80">
                          自定义
                        </span>
                      )}
                    </div>
                    <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground/50">
                      {g.providerId}
                      {g.baseUrl ? ` · ${g.baseUrl}` : ""}
                    </p>
                  </div>
                  <button
                    type="button"
                    className={cn(btnOutline, "flex items-center gap-1 px-2 py-1 text-xs")}
                    title={`从 ${g.providerId} 的 /models 获取模型并勾选添加`}
                    onClick={() =>
                      setFetchTarget({ providerId: g.providerId, providerName: g.name })
                    }
                  >
                    <RefreshCw className="size-3" />
                    获取模型
                  </button>
                  <button
                    type="button"
                    className={cn(btnOutline, "flex items-center gap-1 px-2 py-1 text-xs")}
                    title={`编辑服务商 ${g.providerId}（API Key / API 地址 / 标识）`}
                    onClick={() =>
                      setEditingProvider({
                        providerId: g.providerId,
                        providerName: g.name,
                        predefined: g.predefined,
                        baseUrl: g.baseUrl,
                      })
                    }
                  >
                    <Pencil className="size-3" />
                    编辑
                  </button>
                  <button
                    type="button"
                    className={cn(btnOutline, "px-2 py-1 text-xs")}
                    onClick={() => setAddModel(true, { provider: g.providerId })}
                  >
                    + 添加模型
                  </button>
                  {!g.predefined && (
                    <button
                      type="button"
                      className="flex cursor-pointer items-center gap-1 rounded-lg border border-destructive/50 px-2 py-1 text-xs text-destructive transition-colors hover:bg-destructive/10"
                      title={`删除服务商 ${g.providerId}（含全部模型与凭据）`}
                      onClick={() =>
                        setDeleting({
                          type: "provider",
                          providerId: g.providerId,
                          providerName: g.name,
                        })
                      }
                    >
                      <Trash2 className="size-3" />
                      删除
                    </button>
                  )}
                </div>

                {/* Model list */}
                <div className="space-y-1">
                  {g.models.map((m) => (
                    <div
                      key={m.display}
                      className="group/item flex items-center gap-2 rounded-lg border border-border/50 px-3 py-2 hover:bg-muted/40"
                    >
                      <button
                        type="button"
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left"
                        title={m.active ? "当前默认模型" : "设为默认模型"}
                        onClick={() => {
                          if (!m.active) {
                            switchModel(m.display);
                          }
                        }}
                      >
                        <span
                          className={cn(
                            "size-1.5 shrink-0 rounded-full",
                            m.active ? "bg-primary" : "bg-muted-foreground/30",
                          )}
                        />
                        <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground/90">
                          {m.model_id}
                        </span>
                        <span className="shrink-0 text-xs text-muted-foreground/50">
                          {m.context_window
                            ? `${(m.context_window / 1000).toFixed(0)}K`
                            : ""}
                        </span>
                        {m.active && (
                          <span className="shrink-0 rounded-full bg-primary/10 px-1.5 py-px text-[10px] font-medium text-primary/80">
                            默认
                          </span>
                        )}
                      </button>
                      <button
                        type="button"
                        className="cursor-pointer rounded p-1 text-muted-foreground/40 opacity-70 transition-all group-hover/item:opacity-100 hover:bg-primary/10 hover:text-primary"
                        title="编辑模型参数（上下文窗口 / 输出上限 / 视觉）"
                        onClick={() => setEditingModel(m)}
                      >
                        <Pencil className="size-3.5" />
                      </button>
                      <button
                        type="button"
                        className="cursor-pointer rounded p-1 text-muted-foreground/40 opacity-0 transition-all group-hover/item:opacity-100 hover:bg-destructive/10 hover:text-destructive"
                        title="删除模型"
                        onClick={() => setDeleting({ type: "model", model: m })}
                      >
                        <Trash2 className="size-3.5" />
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>

      {/* Delete confirmation */}
      {deleting && <DeleteConfirm target={deleting} onClose={() => setDeleting(null)} />}

      {/* Fetch-models multi-select dialog */}
      {fetchTarget && (
        <FetchModelsDialog target={fetchTarget} onClose={() => setFetchTarget(null)} />
      )}

      {/* Edit dialogs */}
      {editingModel && (
        <EditModelDialog model={editingModel} onClose={() => setEditingModel(null)} />
      )}
      {editingProvider && (
        <EditProviderDialog
          target={editingProvider}
          onClose={() => setEditingProvider(null)}
        />
      )}
    </div>
  );
}

/* ── Delete confirmation ──────────────────────────────────── */
function DeleteConfirm({
  target,
  onClose,
}: {
  target:
    | { type: "model"; model: ModelInfo }
    | { type: "provider"; providerId: string; providerName: string };
  onClose: () => void;
}) {
  const deleteModel = useApp((s) => s.deleteModel);
  const deleteProvider = useApp((s) => s.deleteProvider);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const confirm = async () => {
    setBusy(true);
    setError("");
    try {
      if (target.type === "model") {
        await deleteModel(target.model.display);
      } else {
        await deleteProvider(target.providerId);
      }
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const title = target.type === "model" ? "删除模型" : "删除服务商";
  const desc =
    target.type === "model"
      ? `确定删除模型「${target.model.model_id}」吗？删除后不可恢复。`
      : `确定删除服务商「${target.providerName}」吗？其全部模型与 API 凭据将被删除，不可恢复。`;

  return (
    <Overlay onClose={() => !busy && onClose()}>
      <div className="mb-3 flex items-center gap-2.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-destructive/10 ring-1 ring-destructive/30">
          <Trash2 className="size-4.5 text-destructive" />
        </span>
        <h3 className="text-[15px] font-semibold">{title}</h3>
      </div>
      <p className="mb-1 text-sm leading-relaxed text-muted-foreground">{desc}</p>
      <p className="mb-4 font-mono text-xs text-muted-foreground/50">
        {target.type === "model" ? target.model.display : target.providerId}
      </p>
      {error && <p className="mb-3 text-xs text-destructive">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" className={btnOutline} onClick={onClose} disabled={busy}>
          取消
        </button>
        <button
          type="button"
          className="flex cursor-pointer items-center gap-1.5 rounded-lg bg-destructive px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-destructive/90 disabled:cursor-not-allowed disabled:opacity-30"
          onClick={confirm}
          disabled={busy}
        >
          {busy && <Loader2 className="size-3.5 animate-spin" />}
          删除
        </button>
      </div>
    </Overlay>
  );
}

/* ── Fetch-models dialog (multi-select, nothing checked by default) ── */
function FetchModelsDialog({
  target,
  onClose,
}: {
  target: { providerId: string; providerName: string };
  onClose: () => void;
}) {
  const discoverModels = useApp((s) => s.discoverModels);
  const applyDiscoveredModels = useApp((s) => s.applyDiscoveredModels);
  /** Null while the list is being fetched */
  const [models, setModels] = useState<DiscoveredModel[] | null>(null);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [fetchError, setFetchError] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const fetchList = useCallback(async () => {
    setModels(null);
    setFetchError("");
    setChecked(new Set());
    try {
      // No api_key: the server uses the saved credentials of the configured provider
      const result = await discoverModels({ provider_id: target.providerId });
      setModels(result.models);
    } catch (e) {
      setFetchError(e instanceof Error ? e.message : String(e));
    }
  }, [discoverModels, target.providerId]);

  useEffect(() => {
    void fetchList();
  }, [fetchList]);

  const toggle = (id: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const selectable = models?.filter((m) => !m.already_added) ?? [];
  const allChecked = selectable.length > 0 && selectable.every((m) => checked.has(m.id));
  const toggleAll = () => {
    setChecked(
      allChecked
        ? new Set()
        : new Set(selectable.map((m) => m.id)),
    );
  };

  const confirm = async () => {
    setBusy(true);
    setError("");
    try {
      await applyDiscoveredModels(target.providerId, [...checked]);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Overlay onClose={() => !busy && onClose()} wide>
      <div className="mb-3 flex items-center gap-2.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-primary/10 ring-1 ring-primary/30">
          <RefreshCw className="size-4.5 text-primary" />
        </span>
        <div className="min-w-0">
          <h3 className="text-[15px] font-semibold">获取模型</h3>
          <p className="truncate font-mono text-xs text-muted-foreground/60">
            {target.providerName}（{target.providerId}）
          </p>
        </div>
      </div>

      {fetchError ? (
        <div className="space-y-3">
          <p className="text-sm text-destructive">{fetchError}</p>
          <button type="button" className={btnOutline} onClick={() => void fetchList()}>
            重试
          </button>
        </div>
      ) : models === null ? (
        <p className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          正在获取模型列表…
        </p>
      ) : models.length === 0 ? (
        <p className="py-6 text-sm text-muted-foreground">服务商未返回任何模型</p>
      ) : (
        <>
          <p className="mb-2 text-xs text-muted-foreground/60">
            勾选要加入模型列表的模型（已在列表中的不可重复添加）
          </p>
          <div className="max-h-80 space-y-1 overflow-y-auto pr-1">
            {models.map((m) => (
              <label
                key={m.id}
                className={cn(
                  "flex items-center gap-2.5 rounded-lg border border-border/50 px-3 py-2 text-sm",
                  m.already_added
                    ? "cursor-not-allowed opacity-50"
                    : "cursor-pointer hover:bg-muted/40",
                )}
              >
                <input
                  type="checkbox"
                  className="size-4 accent-primary"
                  disabled={m.already_added || busy}
                  checked={m.already_added || checked.has(m.id)}
                  onChange={() => toggle(m.id)}
                />
                <span className="min-w-0 flex-1 truncate font-mono">{m.id}</span>
                {m.context_window ? (
                  <span className="shrink-0 text-xs text-muted-foreground/50">
                    {(m.context_window / 1000).toFixed(0)}K
                  </span>
                ) : null}
                {m.already_added && (
                  <span className="shrink-0 text-xs text-muted-foreground/60">已在列表</span>
                )}
              </label>
            ))}
          </div>
        </>
      )}

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <div className="mt-4 flex justify-between">
        <button
          type="button"
          className={btnOutline}
          onClick={toggleAll}
          disabled={models === null || models.length === 0 || busy}
        >
          {allChecked ? "取消全选" : "全选"}
        </button>
        <div className="flex gap-2">
          <button type="button" className={btnOutline} onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className={btnPrimary}
            onClick={confirm}
            disabled={checked.size === 0 || busy}
          >
            {busy && <Loader2 className="mr-1.5 size-3.5 animate-spin" />}
            添加所选（{checked.size}）
          </button>
        </div>
      </div>
    </Overlay>
  );
}

/* ── Edit model dialog (parameters + optional rename) ─────── */
/** Parse a positive integer: empty → undefined (keep the effective value) */
function parsePositive(raw: string, label: string): number | undefined {
  const v = raw.trim();
  if (!v) return undefined;
  if (!/^\d+$/.test(v) || Number(v) === 0) throw new Error(`${label}必须为正整数`);
  return Number(v);
}

function EditModelDialog({ model, onClose }: { model: ModelInfo; onClose: () => void }) {
  const updateModel = useApp((s) => s.updateModel);
  const [modelId, setModelId] = useState(model.model_id);
  const [contextWindow, setContextWindow] = useState(
    model.context_window ? String(model.context_window) : "",
  );
  const [maxTokens, setMaxTokens] = useState(
    model.max_tokens ? String(model.max_tokens) : "",
  );
  const [supportsVision, setSupportsVision] = useState(model.supports_vision ?? false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const submit = async () => {
    setError("");
    const id = modelId.trim();
    if (!id) {
      setError("模型名称不能为空");
      return;
    }
    let cw: number | undefined;
    let mt: number | undefined;
    try {
      cw = parsePositive(contextWindow, "上下文窗口");
      mt = parsePositive(maxTokens, "输出上限");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }
    const rename = id !== model.model_id;
    const unchanged =
      !rename &&
      cw === model.context_window &&
      mt === model.max_tokens &&
      supportsVision === (model.supports_vision ?? false);
    if (unchanged) {
      onClose();
      return;
    }
    setBusy(true);
    try {
      await updateModel({
        selector: model.display,
        ...(rename ? { model_id: id } : {}),
        ...(cw !== undefined ? { context_window: cw } : {}),
        ...(mt !== undefined ? { max_tokens: mt } : {}),
        supports_vision: supportsVision,
      });
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Overlay onClose={() => !busy && onClose()} wide closeOnMask={false}>
      <div className="mb-4 flex items-center gap-2.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-primary/10 ring-1 ring-primary/30">
          <Pencil className="size-4.5 text-primary" />
        </span>
        <div className="min-w-0">
          <h3 className="text-[15px] font-semibold">编辑模型</h3>
          <p className="truncate font-mono text-xs text-muted-foreground/60">
            {model.display}
          </p>
        </div>
      </div>

      <div className="space-y-3">
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">模型名称 (Model ID)</p>
          <input
            value={modelId}
            className={inputCls}
            onChange={(e) => setModelId(e.target.value)}
          />
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <p className="mb-1.5 text-xs font-medium text-muted-foreground">上下文窗口大小</p>
            <input
              value={contextWindow}
              className={inputCls}
              inputMode="numeric"
              placeholder="留空保持不变"
              onChange={(e) => setContextWindow(e.target.value)}
            />
          </div>
          <div>
            <p className="mb-1.5 text-xs font-medium text-muted-foreground">输出上限</p>
            <input
              value={maxTokens}
              className={inputCls}
              inputMode="numeric"
              placeholder="留空保持不变"
              onChange={(e) => setMaxTokens(e.target.value)}
            />
          </div>
        </div>
        <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border border-border/50 px-3 py-2 hover:bg-muted/40">
          <input
            type="checkbox"
            checked={supportsVision}
            className="mt-0.5 size-3.5 cursor-pointer accent-[var(--primary)]"
            onChange={(e) => setSupportsVision(e.target.checked)}
          />
          <span className="text-sm">
            支持图片输入（视觉）
            <span className="block text-xs text-muted-foreground/60">
              关闭后附件会降级为文件名提示文本，不发送图片数据
            </span>
          </span>
        </label>
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <div className="mt-4 flex justify-end gap-2">
        <button type="button" className={btnOutline} onClick={onClose} disabled={busy}>
          取消
        </button>
        <button type="button" className={btnPrimary} onClick={submit} disabled={busy}>
          {busy && <Loader2 className="mr-1.5 size-3.5 animate-spin" />}
          保存
        </button>
      </div>
    </Overlay>
  );
}

/* ── Edit provider dialog (credentials / endpoint / rename) ── */
function EditProviderDialog({
  target,
  onClose,
}: {
  target: { providerId: string; providerName: string; predefined: boolean; baseUrl?: string };
  onClose: () => void;
}) {
  const updateProvider = useApp((s) => s.updateProvider);
  const providers = useApp((s) => s.providers);
  /** Unconfigured predefined provider: it needs an API key before it can be enabled */
  const needsSetup = providers.find((p) => p.id === target.providerId)?.needs_setup ?? false;
  const canRename = !target.predefined;
  const [newId, setNewId] = useState(target.providerId);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(target.baseUrl ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const submit = async () => {
    setError("");
    const id = newId.trim();
    if (canRename && !id) {
      setError("服务商标识不能为空");
      return;
    }
    if (needsSetup && !apiKey.trim()) {
      setError("该服务商尚未配置 API Key，请填写后保存");
      return;
    }
    if (!target.predefined && !baseUrl.trim()) {
      setError("自定义服务商必须填写 API 地址");
      return;
    }
    setBusy(true);
    try {
      // Only send base_url when it changed: prefilled builtin endpoints of unconfigured
      // predefined providers must not become explicit overrides
      const url = baseUrl.trim();
      const urlChanged = url !== (target.baseUrl ?? "").trim();
      await updateProvider({
        provider_id: target.providerId,
        ...(canRename && id !== target.providerId ? { new_id: id } : {}),
        ...(apiKey.trim() ? { api_key: apiKey.trim() } : {}),
        ...(urlChanged ? { base_url: url } : {}),
      });
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Overlay onClose={() => !busy && onClose()} wide closeOnMask={false}>
      <div className="mb-4 flex items-center gap-2.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-primary/10 ring-1 ring-primary/30">
          <Pencil className="size-4.5 text-primary" />
        </span>
        <div className="min-w-0">
          <h3 className="text-[15px] font-semibold">编辑服务商</h3>
          <p className="truncate font-mono text-xs text-muted-foreground/60">
            {target.providerName} · {target.providerId}
          </p>
        </div>
      </div>

      <div className="space-y-3">
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">服务商标识</p>
          <input
            value={newId}
            className={cn(inputCls, !canRename && "opacity-60")}
            disabled={!canRename}
            placeholder="例如: ollama、siliconflow"
            onChange={(e) => setNewId(e.target.value)}
          />
          <p className="mt-1 text-xs text-muted-foreground/50">
            {canRename
              ? "改名后该服务商下的模型选择器同步迁移"
              : "预定义服务商不可改名"}
          </p>
        </div>
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">API Key</p>
          <input
            type="password"
            value={apiKey}
            className={inputCls}
            placeholder={needsSetup ? "该服务商尚未配置，请填写" : "留空则不修改"}
            onChange={(e) => setApiKey(e.target.value)}
          />
        </div>
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">API 地址</p>
          <input
            value={baseUrl}
            className={inputCls}
            placeholder={
              target.predefined ? "留空使用内置端点" : "例如: http://localhost:11434/v1"
            }
            onChange={(e) => setBaseUrl(e.target.value)}
          />
          <p className="mt-1 text-xs text-muted-foreground/50">
            {target.predefined
              ? "清空则回退内置端点（可用于指向代理 / 中转）"
              : "OpenAI 兼容端点，须以 http:// 或 https:// 开头"}
          </p>
        </div>
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <div className="mt-4 flex justify-end gap-2">
        <button type="button" className={btnOutline} onClick={onClose} disabled={busy}>
          取消
        </button>
        <button type="button" className={btnPrimary} onClick={submit} disabled={busy}>
          {busy && <Loader2 className="mr-1.5 size-3.5 animate-spin" />}
          保存
        </button>
      </div>
    </Overlay>
  );
}

/* ── Add model dialog (single-page form, credential fields shown conditionally) ── */
const NEW_PROVIDER = "__new__";

export function AddModelDialog() {
  const show = useApp((s) => s.showAddModel);
  /** Prefill (set when opened from a provider card) */
  const preset = useApp((s) => s.addModelPreset);
  const providers = useApp((s) => s.providers);
  const addModel = useApp((s) => s.addModel);
  const discoverModels = useApp((s) => s.discoverModels);
  const setAddModel = useApp((s) => s.setAddModel);

  const [providerId, setProviderId] = useState("");
  const [modelId, setModelId] = useState("");
  const [contextWindow, setContextWindow] = useState(DEFAULT_CONTEXT_WINDOW);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [newProviderName, setNewProviderName] = useState("");
  /** Models reported by the provider's `/models` (after the fetch button); null = not fetched */
  const [discovered, setDiscovered] = useState<DiscoveredModel[] | null>(null);
  const [fetching, setFetching] = useState(false);
  const [discoverNote, setDiscoverNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // Sync the prefill on open (useState initial values only apply on first mount)
  useEffect(() => {
    if (show) {
      setProviderId(preset?.provider ?? "");
      setModelId(preset?.modelId ?? "");
    }
  }, [show, preset]);

  // A different provider means a different model list
  useEffect(() => {
    setDiscovered(null);
    setDiscoverNote("");
  }, [providerId, newProviderName]);

  if (!show) return null;

  const isNewProvider = providerId === NEW_PROVIDER;
  const selected = providers.find((p) => p.id === providerId);
  // Unconfigured predefined provider: supplying an API key is enough (base_url comes from the built-in endpoint)
  const needsApiKey = isNewProvider || !!selected?.needs_setup;
  const needsBaseUrl = isNewProvider;
  const targetProvider = isNewProvider ? newProviderName.trim() : providerId;
  // The list needs either the typed credentials or a provider the server already knows
  const canDiscover =
    !!targetProvider &&
    (!needsApiKey || !!apiKey.trim()) &&
    (!needsBaseUrl || !!baseUrl.trim());

  const close = () => {
    if (busy) return;
    setAddModel(false);
    setProviderId("");
    setModelId("");
    setContextWindow(DEFAULT_CONTEXT_WINDOW);
    setApiKey("");
    setBaseUrl("");
    setNewProviderName("");
    setDiscovered(null);
    setDiscoverNote("");
    setError("");
  };

  /** Pull the provider's `/models` list: with the typed credentials for new/unconfigured
   * providers, or through the server's saved credentials for configured ones */
  const doDiscover = async () => {
    setFetching(true);
    setError("");
    setDiscoverNote("");
    try {
      const result = await discoverModels({
        provider_id: targetProvider || undefined,
        base_url: needsBaseUrl ? baseUrl.trim() : undefined,
        api_key: needsApiKey ? apiKey.trim() : undefined,
      });
      setDiscovered(result.models);
      if (result.models.length === 0) setDiscoverNote("服务商未返回任何模型，请手动输入");
    } catch (e) {
      // Drop the stale list so the error message is not shown next to outdated rows
      setDiscovered(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setFetching(false);
    }
  };

  const submit = async () => {
    setError("");
    if (!targetProvider) {
      setError(isNewProvider ? "服务商名称不能为空" : "请选择服务商");
      return;
    }
    if (!modelId.trim()) {
      setError("模型名称不能为空");
      return;
    }
    if (needsApiKey && !apiKey.trim()) {
      setError("API Key 不能为空");
      return;
    }
    if (needsBaseUrl && !baseUrl.trim()) {
      setError("API 地址不能为空");
      return;
    }
    const cw = contextWindow.trim();
    if (cw && (!/^\d+$/.test(cw) || Number(cw) === 0)) {
      setError("上下文窗口必须为正整数");
      return;
    }
    // A model picked from the fetched list: leave the window open so the server materializes
    // the full provider metadata (window / output cap / vision) instead of a bare override
    const picked = discovered?.find((d) => d.id === modelId.trim());
    const contextWindowOpen =
      picked?.context_window !== undefined && String(picked.context_window) === cw;

    setBusy(true);
    try {
      await addModel({
        provider_id: targetProvider,
        model_id: modelId.trim(),
        context_window: contextWindowOpen ? undefined : cw ? Number(cw) : undefined,
        base_url: needsBaseUrl && baseUrl.trim() ? baseUrl.trim() : undefined,
        api_key: needsApiKey && apiKey.trim() ? apiKey.trim() : undefined,
      });
      // Success: the store already closed the dialog, reset the form
      setProviderId("");
      setModelId("");
      setContextWindow(DEFAULT_CONTEXT_WINDOW);
      setApiKey("");
      setBaseUrl("");
      setNewProviderName("");
      setDiscovered(null);
      setDiscoverNote("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Overlay onClose={close} wide closeOnMask={false}>
      <h3 className="mb-1 text-[15px] font-semibold">添加模型</h3>
      <p className="mb-4 text-xs text-muted-foreground/60">
        添加后自动切换到新模型；已配置服务商复用现有凭据
      </p>

      {/* Provider select */}
      <p className="mb-1.5 text-xs font-medium text-muted-foreground">服务商</p>
      <select
        value={providerId}
        className={cn(inputCls, "cursor-pointer")}
        onChange={(e) => setProviderId(e.target.value)}
      >
        <option value="" disabled>
          选择服务商…
        </option>
        {providers.map((p: ProviderOption) => (
          <option key={p.id} value={p.id}>
            {p.name}（{p.id}）{p.needs_setup ? " · 需配置 API Key" : ""}
          </option>
        ))}
        <option value={NEW_PROVIDER}>+ 新建自定义端点</option>
      </select>

      {/* Credential fields: new endpoint = name + URL + key; unconfigured predefined provider = key only;
          configured providers reuse credentials (mirrors the TUI: no credential inputs) */}
      {isNewProvider && (
        <div className="mt-3 space-y-3">
          <div>
            <p className="mb-1.5 text-xs font-medium text-muted-foreground">服务商名称</p>
            <input
              value={newProviderName}
              className={inputCls}
              placeholder="用于标识，如 ollama、siliconflow"
              onChange={(e) => setNewProviderName(e.target.value)}
            />
          </div>
          <div>
            <p className="mb-1.5 text-xs font-medium text-muted-foreground">API 地址</p>
            <input
              value={baseUrl}
              className={inputCls}
              placeholder="例如: http://localhost:11434/v1"
              onChange={(e) => setBaseUrl(e.target.value)}
            />
          </div>
        </div>
      )}
      {needsApiKey && (
        <div className="mt-3">
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">API Key</p>
          <input
            type="password"
            value={apiKey}
            className={inputCls}
            placeholder="sk-…"
            onChange={(e) => setApiKey(e.target.value)}
          />
          {selected?.needs_setup && (
            <p className="mt-1 truncate text-xs text-muted-foreground/50">
              端点: {selected.base_url}
            </p>
          )}
        </div>
      )}

      {/* Model fields: fetch the provider's list to pick from, or type the id */}
      <div className="mt-3 space-y-3">
        <div>
          <div className="mb-1.5 flex items-center justify-between">
            <p className="text-xs font-medium text-muted-foreground">模型名称 (Model ID)</p>
            {canDiscover && (
              <button
                type="button"
                className={cn(
                  btnOutline,
                  "flex items-center gap-1 px-2 py-0.5 text-xs",
                  discovered && "text-primary",
                )}
                title="从服务商的 /models 获取模型列表"
                disabled={fetching || busy}
                onClick={() => void doDiscover()}
              >
                {fetching ? (
                  <Loader2 className="size-3 animate-spin" />
                ) : (
                  <RefreshCw className="size-3" />
                )}
                获取模型列表
              </button>
            )}
          </div>
          {discovered && discovered.length > 0 && (
            <select
              value={discovered.some((d) => d.id === modelId.trim()) ? modelId.trim() : ""}
              className={cn(inputCls, "mb-2 cursor-pointer")}
              onChange={(e) => {
                const d = discovered.find((x) => x.id === e.target.value);
                if (!d) return;
                setModelId(d.id);
                if (d.context_window) setContextWindow(String(d.context_window));
              }}
            >
              <option value="" disabled>
                从服务商列表选择…
              </option>
              {discovered.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name && d.name !== d.id ? `${d.name} · ${d.id}` : d.id}
                  {d.context_window ? ` (${Math.round(d.context_window / 1000)}K)` : ""}
                </option>
              ))}
            </select>
          )}
          <input
            value={modelId}
            className={inputCls}
            placeholder="例如: qwen3-235b-a22b, llama3-70b"
            onChange={(e) => setModelId(e.target.value)}
          />
          {discoverNote && (
            <p className="mt-1 text-xs text-muted-foreground/50">{discoverNote}</p>
          )}
        </div>
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">上下文窗口大小</p>
          <input
            value={contextWindow}
            className={inputCls}
            inputMode="numeric"
            placeholder={`留空使用默认值 ${DEFAULT_CONTEXT_WINDOW} (256K)`}
            onChange={(e) => setContextWindow(e.target.value)}
          />
        </div>
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}

      <div className="mt-4 flex justify-end gap-2">
        <button type="button" className={btnOutline} onClick={close} disabled={busy}>
          取消
        </button>
        <button type="button" className={btnPrimary} onClick={submit} disabled={busy}>
          {busy && <Loader2 className="mr-1.5 size-3.5 animate-spin" />}
          添加并切换
        </button>
      </div>
    </Overlay>
  );
}
