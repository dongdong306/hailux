// 设置面板：内联视图（替换聊天区渲染）。当前含「模型提供商」区块：
// provider 卡片（凭据信息 + 模型列表），支持添加模型 / 新建服务商 /
// 删除模型 / 删除服务商 / 设为默认模型。后续可扩展更多设置区块。
import { useEffect, useState } from "react";
import {
  ArrowLeft,
  Loader2,
  Plus,
  Settings,
  Trash2,
} from "lucide-react";
import { useApp } from "../store/app-store";
import { cn } from "../lib/utils";
import type { ModelInfo, ProviderOption } from "../runtime/types";
import { Overlay } from "./dialogs";

/* 按钮样式（与 dialogs.tsx 一致） */
const btnPrimary =
  "cursor-pointer rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30";
const btnOutline =
  "cursor-pointer rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-30";

const inputCls =
  "w-full rounded-lg border border-border bg-card px-3 py-2 text-sm outline-none transition-colors focus:border-primary/30 focus:ring-2 focus:ring-ring/30";

/** 上下文窗口默认值 */
const DEFAULT_CONTEXT_WINDOW = "131072";

/* ── 设置面板 ─────────────────────────────────────────────── */
export function SettingsView() {
  const setView = useApp((s) => s.setView);
  const models = useApp((s) => s.models);
  const providers = useApp((s) => s.providers);
  const switchModel = useApp((s) => s.switchModel);
  const setAddModel = useApp((s) => s.setAddModel);
  /** 删除目标：单个模型 / 整个 provider */
  const [deleting, setDeleting] = useState<
    | { type: "model"; model: ModelInfo }
    | { type: "provider"; providerId: string; providerName: string }
    | null
  >(null);

  // providers（已配置）与 models 分组对齐；models 里可能含未配置 provider 的
  // 预定义条目（needs_setup），单独归入「未配置」组展示。
  // 契约：models 由后端 available_models 按 provider 连续返回（BTreeMap 字母序），
  // 此处按相邻分组聚合；若后端排序不再连续，需改为按 providerId 累积分组
  const configuredIds = new Set(providers.map((p) => p.id));
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

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 顶部栏 */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border/40 px-4">
        <button
          type="button"
          onClick={() => setView("chat")}
          aria-label="返回对话"
          className="flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <ArrowLeft className="size-4" />
        </button>
        <Settings className="size-4 text-muted-foreground/70" />
        <h2 className="text-sm font-semibold text-foreground">设置</h2>
      </div>

      {/* 内容区 */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-3xl px-4 py-6">
          {/* ── 模型提供商区块 ── */}
          <div className="mb-3 flex items-center justify-between">
            <div>
              <h3 className="text-[15px] font-semibold text-foreground">模型提供商</h3>
              <p className="mt-0.5 text-xs text-muted-foreground/60">
                管理 API 服务商与模型；预定义服务商的预定义模型不可删除
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
                {/* 卡片头：服务商信息 + 操作 */}
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
                    className={cn(btnOutline, "px-2 py-1 text-xs")}
                    onClick={() => setAddModel(true, { provider: g.providerId })}
                  >
                    + 添加模型
                  </button>
                  {!g.predefined && configuredIds.has(g.providerId) && (
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

                {/* 模型列表 */}
                <div className="space-y-1">
                  {g.models.map((m) => (
                    <div
                      key={m.display}
                      className="group/item flex items-center gap-2 rounded-lg border border-border/50 px-3 py-2 hover:bg-muted/40"
                    >
                      <button
                        type="button"
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left"
                        title={
                          m.needs_setup
                            ? "配置 API Key 以启用该服务商"
                            : m.active
                              ? "当前默认模型"
                              : "设为默认模型"
                        }
                        onClick={() => {
                          if (m.needs_setup) {
                            // 未配置 provider：打开添加弹窗预填，补 API Key 启用
                            setAddModel(true, {
                              provider: m.provider_id,
                              modelId: m.model_id,
                            });
                          } else if (!m.active) {
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
                        {m.needs_setup && (
                          <span className="shrink-0 text-xs text-warning">(需配置)</span>
                        )}
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
                      {m.deletable && (
                        <button
                          type="button"
                          className="cursor-pointer rounded p-1 text-muted-foreground/40 opacity-0 transition-all group-hover/item:opacity-100 hover:bg-destructive/10 hover:text-destructive"
                          title="删除模型"
                          onClick={() => setDeleting({ type: "model", model: m })}
                        >
                          <Trash2 className="size-3.5" />
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>

      {/* 删除二次确认 */}
      {deleting && <DeleteConfirm target={deleting} onClose={() => setDeleting(null)} />}
    </div>
  );
}

/* ── 删除确认 ─────────────────────────────────────────────── */
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

/* ── 添加模型弹窗（单页表单，动态显隐凭据字段）───────────── */
const NEW_PROVIDER = "__new__";

export function AddModelDialog() {
  const show = useApp((s) => s.showAddModel);
  /** 预填项（provider 卡片 / 需配置模型入口打开时设置） */
  const preset = useApp((s) => s.addModelPreset);
  const providers = useApp((s) => s.providers);
  const addModel = useApp((s) => s.addModel);
  const setAddModel = useApp((s) => s.setAddModel);

  const [providerId, setProviderId] = useState("");
  const [modelId, setModelId] = useState("");
  const [contextWindow, setContextWindow] = useState(DEFAULT_CONTEXT_WINDOW);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [newProviderName, setNewProviderName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 打开时同步预填项（useState 初始值只在首次挂载时生效）
  useEffect(() => {
    if (show) {
      setProviderId(preset?.provider ?? "");
      setModelId(preset?.modelId ?? "");
    }
  }, [show, preset]);

  if (!show) return null;

  const isNewProvider = providerId === NEW_PROVIDER;
  const selected = providers.find((p) => p.id === providerId);
  // 未配置的预定义 provider：仅补 API Key 即可启用（base_url 用预定义端点）
  const needsApiKey = isNewProvider || !!selected?.needs_setup;
  const needsBaseUrl = isNewProvider;

  const close = () => {
    if (busy) return;
    setAddModel(false);
    // 重置表单
    setProviderId("");
    setModelId("");
    setContextWindow(DEFAULT_CONTEXT_WINDOW);
    setApiKey("");
    setBaseUrl("");
    setNewProviderName("");
    setError("");
  };

  const submit = async () => {
    setError("");
    const targetProvider = isNewProvider ? newProviderName.trim() : providerId;
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

    setBusy(true);
    try {
      await addModel({
        provider_id: targetProvider,
        model_id: modelId.trim(),
        context_window: cw ? Number(cw) : undefined,
        base_url: needsBaseUrl && baseUrl.trim() ? baseUrl.trim() : undefined,
        api_key: needsApiKey && apiKey.trim() ? apiKey.trim() : undefined,
      });
      // 成功：store 已关闭弹窗，重置表单
      setProviderId("");
      setModelId("");
      setContextWindow(DEFAULT_CONTEXT_WINDOW);
      setApiKey("");
      setBaseUrl("");
      setNewProviderName("");
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

      {/* 服务商选择 */}
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

      {/* 凭据字段：新建端点 = 名称+地址+Key；未配置预定义服务商 = 仅 Key；
          已配置服务商复用凭据（对齐 TUI：不显示凭据输入） */}
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

      {/* 模型字段 */}
      <div className="mt-3 space-y-3">
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">模型名称 (Model ID)</p>
          <input
            value={modelId}
            className={inputCls}
            placeholder="例如: qwen3-235b-a22b, llama3-70b"
            onChange={(e) => setModelId(e.target.value)}
          />
        </div>
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">上下文窗口大小</p>
          <input
            value={contextWindow}
            className={inputCls}
            inputMode="numeric"
            placeholder={`留空使用默认值 ${DEFAULT_CONTEXT_WINDOW} (128K)`}
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
