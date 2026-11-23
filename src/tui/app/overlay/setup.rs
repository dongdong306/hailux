use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::config::model_discovery::{self, RemoteModel};
use crate::config::{CustomModelEntry, DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};
use crate::tui::event::AppEvent;
use crate::tui::setup::{ProviderKind, SetupForm, SetupStep};

impl App {
    pub(in crate::tui::app) fn handle_setup_event(&mut self, event: AppEvent) -> Result<()> {
        let key = match event {
            AppEvent::InputKey(k) => k,
            AppEvent::InputPaste(text) => {
                if let AppState::Setup(ref mut form) = self.state
                    && form.is_text_step()
                {
                    let sanitized: String =
                        text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
                    form.buffer.insert_str(form.cursor, &sanitized);
                    form.cursor += sanitized.len();
                    form.error_msg.clear();
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        if key.code == KeyCode::Esc {
            return self.setup_go_back();
        }

        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::Setup(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };

        form.error_msg.clear();

        match form.step {
            SetupStep::Welcome => {
                if key.code == KeyCode::Enter {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = 0;
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::SelectProvider => {
                let total = form.provider_options.len() + 1;
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        match form.provider_options.get(form.selected_index) {
                            Some(p) => {
                                form.provider_id = p.id.clone();
                                form.provider_kind = p.kind;
                                form.base_url = p.base_url.clone();
                                form.step = match p.kind {
                                    // Credentials already known: go straight to the model list
                                    ProviderKind::Configured => SetupStep::FetchingModels,
                                    _ => SetupStep::InputApiKey,
                                };
                            }
                            None => {
                                form.provider_kind = ProviderKind::Custom;
                                form.step = SetupStep::InputProviderName;
                            }
                        }
                        form.api_key.clear();
                        form.remote_models.clear();
                        form.model_id.clear();
                        form.context_window.clear();
                        form.checked.clear();
                        form.existing_ids.clear();
                        form.added_count = 0;
                        form.default_model.clear();
                        form.selected_index = 0;
                        form.buffer.clear();
                        form.cursor = 0;
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
                if self.state_is_fetching() {
                    self.setup_start_fetch()?;
                }
            }
            // Keys are ignored while the request is in flight (Esc goes back)
            SetupStep::FetchingModels => {
                self.state = AppState::Setup(form);
            }
            SetupStep::SelectModel => {
                let total = form.model_choice_count();
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(m) = form.remote_models.get(form.selected_index) {
                            let id = m.id.clone();
                            if form.checked.contains(&id) {
                                form.checked.remove(&id);
                            } else {
                                form.checked.insert(id);
                            }
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        if form.checked.len() == form.remote_models.len() {
                            form.checked.clear();
                        } else {
                            form.checked =
                                form.remote_models.iter().map(|m| m.id.clone()).collect();
                        }
                    }
                    KeyCode::Enter => {
                        if form.selected_index < form.remote_models.len() {
                            let picked = form.picked_ids();
                            if picked.is_empty() {
                                form.error_msg = "请先用 Space 勾选要添加的模型".to_string();
                            } else {
                                self.state = AppState::Setup(form);
                                return self.setup_apply_picked();
                            }
                        } else {
                            form.step = SetupStep::InputModelName;
                            form.buffer.clear();
                            form.cursor = 0;
                        }
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::Done => {
                if key.code == KeyCode::Enter {
                    return self.setup_finish(form);
                }
                self.state = AppState::Setup(form);
            }
            _ => {
                let is_context_window = form.step == SetupStep::InputContextWindow;
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.cursor = next;
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    KeyCode::Enter => {
                        if form.buffer.is_empty() && !is_context_window {
                            form.error_msg = match form.step {
                                SetupStep::InputApiKey => "API Key 不能为空".to_string(),
                                SetupStep::InputProviderName => "服务商名称不能为空".to_string(),
                                SetupStep::InputBaseUrl => "API 地址不能为空".to_string(),
                                SetupStep::InputModelName => "模型名称不能为空".to_string(),
                                _ => String::new(),
                            };
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        if is_context_window
                            && !form.buffer.is_empty()
                            && form.buffer.parse::<u32>().is_err()
                        {
                            form.error_msg = "请输入有效的数字".to_string();
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        match form.step {
                            SetupStep::InputProviderName => {
                                form.provider_id = form.buffer.trim().to_string();
                                form.step = SetupStep::InputBaseUrl;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::InputBaseUrl => {
                                let url = form.buffer.trim().to_string();
                                if !url.starts_with("http://") && !url.starts_with("https://") {
                                    form.error_msg =
                                        "API 地址必须以 http:// 或 https:// 开头".to_string();
                                    self.state = AppState::Setup(form);
                                    return Ok(());
                                }
                                form.base_url = url;
                                form.step = SetupStep::InputApiKey;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::InputApiKey => {
                                form.api_key = form.buffer.trim().to_string();
                                self.state = AppState::Setup(form);
                                self.setup_start_fetch()?;
                                return Ok(());
                            }
                            SetupStep::InputModelName => {
                                form.model_id = form.buffer.trim().to_string();
                                form.step = SetupStep::InputContextWindow;
                                form.buffer = DEFAULT_CONTEXT_WINDOW.to_string();
                                form.cursor = form.buffer.len();
                            }
                            SetupStep::InputContextWindow => {
                                form.context_window = form.buffer.clone();
                                form.step = SetupStep::Done;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
        }
        Ok(())
    }

    fn state_is_fetching(&self) -> bool {
        matches!(&self.state, AppState::Setup(f) if f.step == SetupStep::FetchingModels)
    }

    /// SelectModel + Enter with checked rows: add the picked models from the cached
    /// `/models` list and persist. Rows already in the table are skipped by the config layer.
    fn setup_apply_picked(&mut self) -> Result<()> {
        let AppState::Setup(ref mut form) = self.state else {
            return Ok(());
        };
        let provider_id = form.provider_id.clone();
        let picked = form.picked_ids();
        let added = match self.config.apply_discovered_models(&provider_id, &picked) {
            Ok(a) => a,
            Err(e) => {
                form.error_msg = e.to_string();
                return Ok(());
            }
        };
        if let Err(e) = self.config.save() {
            form.error_msg = format!("保存失败: {e}");
            return Ok(());
        }
        // Subagents read the shared config snapshot; keep it in sync
        if let Ok(mut shared) = self.shared.config.lock() {
            *shared = self.config.clone();
        }

        form.checked.clear();
        form.added_count = added.len();
        // First run: the first picked model becomes the default; append mode never switches it
        form.default_model = if form.append_only {
            String::new()
        } else {
            added
                .first()
                .map(|id| format!("{provider_id}/{id}"))
                .unwrap_or_default()
        };
        form.model_id.clear();
        form.context_window.clear();
        form.step = SetupStep::Done;
        Ok(())
    }

    /// Esc: step back one level; from the first step, leave setup (quit on first run,
    /// back to the model picker in append mode)
    fn setup_go_back(&mut self) -> Result<()> {
        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::Setup(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };
        form.error_msg.clear();

        // Where a provider-dependent step returns to: the key prompt, or the provider list
        // when credentials are already known
        let after_provider = |form: &SetupForm| {
            if form.needs_api_key() {
                SetupStep::InputApiKey
            } else {
                SetupStep::SelectProvider
            }
        };
        let select_provider = |form: &mut SetupForm| {
            form.step = SetupStep::SelectProvider;
            form.selected_index = form
                .provider_options
                .iter()
                .position(|p| p.id == form.provider_id)
                .unwrap_or(form.provider_options.len());
            form.buffer.clear();
            form.cursor = 0;
        };

        match form.step {
            SetupStep::Welcome => {
                self.should_quit = true;
            }
            SetupStep::SelectProvider => {
                if form.append_only {
                    self.open_model_picker();
                    return Ok(());
                }
                form.step = SetupStep::Welcome;
                form.buffer.clear();
                form.cursor = 0;
            }
            SetupStep::InputProviderName => {
                form.step = SetupStep::SelectProvider;
                form.selected_index = form.provider_options.len();
                form.buffer.clear();
                form.cursor = 0;
            }
            SetupStep::InputBaseUrl => {
                form.step = SetupStep::InputProviderName;
                form.buffer = form.provider_id.clone();
                form.cursor = form.buffer.len();
            }
            SetupStep::InputApiKey => {
                if form.provider_kind == ProviderKind::Custom {
                    form.step = SetupStep::InputBaseUrl;
                    form.buffer = form.base_url.clone();
                } else {
                    select_provider(&mut form);
                }
                form.cursor = form.buffer.len();
            }
            SetupStep::FetchingModels => {
                form.step = after_provider(&form);
                if form.step == SetupStep::InputApiKey {
                    form.buffer = form.api_key.clone();
                } else {
                    select_provider(&mut form);
                }
                form.cursor = form.buffer.len();
            }
            SetupStep::SelectModel => {
                form.remote_models.clear();
                form.step = after_provider(&form);
                if form.step == SetupStep::InputApiKey {
                    form.buffer = form.api_key.clone();
                } else {
                    select_provider(&mut form);
                }
                form.cursor = form.buffer.len();
            }
            SetupStep::InputModelName => {
                if !form.remote_models.is_empty() {
                    // Came from the "manual input" row of the list
                    form.step = SetupStep::SelectModel;
                    form.buffer.clear();
                    form.cursor = 0;
                } else {
                    form.step = after_provider(&form);
                    if form.step == SetupStep::InputApiKey {
                        form.buffer = form.api_key.clone();
                    } else {
                        select_provider(&mut form);
                    }
                    form.cursor = form.buffer.len();
                }
            }
            SetupStep::InputContextWindow => {
                form.step = SetupStep::InputModelName;
                form.buffer = form.model_id.clone();
                form.cursor = form.buffer.len();
            }
            SetupStep::Done => {
                if form.append_only {
                    // Back to the list the entry came from
                    self.open_model_picker();
                    return Ok(());
                }
                form.step = SetupStep::SelectProvider;
                form.selected_index = if form.provider_kind == ProviderKind::Custom {
                    form.provider_options.len()
                } else {
                    form.provider_options
                        .iter()
                        .position(|p| p.id == form.provider_id)
                        .unwrap_or(0)
                };
            }
        }
        self.state = AppState::Setup(form);
        Ok(())
    }

    /// Kick off the background `/models` fetch for the selected provider. Without usable
    /// credentials the form falls through to manual model input instead.
    fn setup_start_fetch(&mut self) -> Result<()> {
        let AppState::Setup(ref mut form) = self.state else {
            return Ok(());
        };
        let provider_id = form.provider_id.clone();
        let creds = match form.provider_kind {
            ProviderKind::Configured => self.config.providers.get(&provider_id).and_then(|e| {
                self.config
                    .provider_base_url(&provider_id)
                    .map(|url| (url, e.api_key.clone()))
            }),
            _ => Some((form.base_url.clone(), form.api_key.clone())),
        };
        let Some((base_url, api_key)) = creds.filter(|(u, k)| !u.is_empty() && !k.is_empty())
        else {
            form.step = SetupStep::InputModelName;
            form.error_msg = "缺少 API 地址或 API Key，请手动输入模型".to_string();
            form.buffer.clear();
            form.cursor = 0;
            return Ok(());
        };

        form.step = SetupStep::FetchingModels;
        form.error_msg.clear();
        let tx = self.events.0.clone();
        tokio::spawn(async move {
            let result = model_discovery::fetch_provider_models(&base_url, &api_key)
                .await
                .map_err(|e| e.to_string());
            let _ = tx
                .send(AppEvent::ModelsRefreshed {
                    provider_id,
                    result,
                })
                .await;
        });
        Ok(())
    }

    /// Apply the fetch result to the setup form: persist the provider (the picked rows are
    /// added via `apply_discovered_models`, which requires configured credentials), cache the
    /// list and open the multi-select step. A failed fetch falls back to manual input.
    pub(in crate::tui::app) fn handle_setup_models_refreshed(
        &mut self,
        provider_id: &str,
        result: Result<Vec<RemoteModel>, String>,
    ) -> Result<()> {
        let AppState::Setup(ref mut form) = self.state else {
            return Ok(());
        };
        if form.step != SetupStep::FetchingModels || form.provider_id != provider_id {
            return Ok(());
        }
        form.buffer.clear();
        form.cursor = 0;
        match result {
            Ok(models) if !models.is_empty() => {
                // The provider must exist with credentials before rows can be added
                if form.needs_api_key() && !form.api_key.is_empty() {
                    let base_url_override = (form.provider_kind == ProviderKind::Custom)
                        .then_some(form.base_url.as_str());
                    let api_key = form.api_key.clone();
                    if let Err(e) = self.config.update_provider(
                        provider_id,
                        None,
                        Some(&api_key),
                        base_url_override,
                    ) {
                        form.step = SetupStep::InputApiKey;
                        form.buffer = api_key;
                        form.cursor = form.buffer.len();
                        form.error_msg = format!("保存服务商失败: {e}");
                        return Ok(());
                    }
                }
                self.config
                    .set_discovered_models(provider_id, models.clone());
                if let Err(e) = model_discovery::save_cache(&self.config.discovered) {
                    eprintln!("[warn] 无法写入模型列表缓存: {e}");
                }
                if let Err(e) = self.config.save() {
                    form.error_msg = format!("保存失败: {e}");
                }
                form.existing_ids = self
                    .config
                    .providers
                    .get(provider_id)
                    .map(|p| p.models.keys().cloned().collect())
                    .unwrap_or_default();
                form.remote_models = models;
                form.checked.clear();
                form.selected_index = 0;
                form.step = SetupStep::SelectModel;
            }
            Ok(_) => {
                form.step = SetupStep::InputModelName;
                form.error_msg = "服务商未返回任何模型，请手动输入".to_string();
            }
            Err(e) => {
                form.step = SetupStep::InputModelName;
                form.error_msg = format!("获取模型列表失败，请手动输入：{e}");
            }
        }
        Ok(())
    }

    /// Done + Enter: land any hand-typed model (the multi-select rows were already applied on
    /// confirm), save, and — on first run — switch to the default model and return to chat.
    /// Append mode never touches the active model and goes back to the model picker.
    fn setup_finish(&mut self, mut form: SetupForm) -> Result<()> {
        // Manual-input path: the typed model lands here with its context window
        if !form.model_id.is_empty() {
            let context_window = if form.context_window.is_empty() {
                DEFAULT_CONTEXT_WINDOW
            } else {
                match form.context_window.parse::<u32>() {
                    Ok(v) => v,
                    Err(_) => {
                        form.error_msg =
                            format!("context_window 不是有效数字: {}", form.context_window);
                        self.state = AppState::Setup(form);
                        return Ok(());
                    }
                }
            };
            let provider_id = form.provider_id.clone();
            let model_id = form.model_id.trim().to_string();
            if model_id.is_empty() || model_id.contains('/') || provider_id.contains('/') {
                form.error_msg = "模型名称不能为空且不能包含 /".to_string();
                self.state = AppState::Setup(form);
                return Ok(());
            }

            let base_url =
                (form.provider_kind == ProviderKind::Custom).then_some(form.base_url.as_str());
            let api_key = form
                .needs_api_key()
                .then_some(form.api_key.as_str())
                .filter(|k| !k.is_empty());

            // A model already in the table keeps its parameters
            let exists = self
                .config
                .providers
                .get(&provider_id)
                .is_some_and(|p| p.find_model_key(&model_id).is_some());
            if !exists {
                self.config.add_model_entry(
                    &provider_id,
                    base_url,
                    api_key,
                    &model_id,
                    CustomModelEntry {
                        max_tokens: DEFAULT_OUTPUT_TOKENS,
                        context_window,
                        supports_vision: None,
                    },
                );
            }
            if !form.append_only && form.default_model.is_empty() {
                form.default_model = format!("{provider_id}/{model_id}");
            }
        }

        if form.append_only {
            // Save (a no-op when the multi-select already persisted) and reopen the picker
            if let Err(e) = self.config.save() {
                form.error_msg = format!("保存失败: {e}");
                self.state = AppState::Setup(form);
                return Ok(());
            }
            if let Ok(mut shared) = self.shared.config.lock() {
                *shared = self.config.clone();
            }
            self.open_model_picker();
            return Ok(());
        }

        // First run: fall back to the provider's first table row when nothing was picked
        if form.default_model.is_empty() {
            form.default_model = self
                .config
                .providers
                .get(&form.provider_id)
                .and_then(|p| p.models.keys().next())
                .map(|mid| format!("{}/{}", form.provider_id, mid))
                .unwrap_or_default();
        }
        if form.default_model.is_empty() {
            form.error_msg = "没有可用的模型，请先添加".to_string();
            self.state = AppState::Setup(form);
            return Ok(());
        }

        let display = form.default_model.clone();
        match self.config.resolve(&display) {
            Ok(resolved) => {
                self.agent.switch_model(
                    resolved.config.clone(),
                    &resolved.model_id,
                    &resolved.display,
                    resolved.max_tokens,
                    resolved.supports_vision,
                    resolved.native_reasoning,
                );
                self.resolved = resolved;
                self.config.main_model = display.clone();
                if let Err(e) = self.config.save() {
                    form.error_msg = format!("保存失败: {e}");
                    self.state = AppState::Setup(form);
                    return Ok(());
                }
                if let Ok(mut shared) = self.shared.config.lock() {
                    *shared = self.config.clone();
                }
                // State is already Chat (set via mem::replace); keep it
                Ok(())
            }
            Err(e) => {
                form.error_msg = format!("解析模型失败: {e}");
                self.state = AppState::Setup(form);
                Ok(())
            }
        }
    }
}
