use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use super::super::types::ModelPickerAction;
use crate::config::model_discovery::{self, RemoteModel};
use crate::tui::event::AppEvent;
use crate::tui::setup::{ProviderKind, SetupForm, SetupStep};

impl App {
    pub(in crate::tui::app) fn handle_model_picker_event(&mut self, event: AppEvent) -> Result<()> {
        // Background fetch result: can arrive at any time while the picker is open
        if let AppEvent::ModelsRefreshed {
            provider_id,
            result,
        } = event
        {
            return self.handle_models_refreshed(&provider_id, result);
        }

        let action = {
            let AppState::ModelPicker {
                models,
                selected_index,
                refreshing,
                ..
            } = &mut self.state
            else {
                return Ok(());
            };

            // Total rows = model count + 1 (the "Add model..." row)
            let total_items = models.len() + 1;

            let AppEvent::InputKey(key) = event else {
                return Ok(());
            };

            match key.code {
                KeyCode::Esc => Some(ModelPickerAction::Close),
                KeyCode::Up => {
                    if *selected_index > 0 {
                        *selected_index -= 1;
                    }
                    None
                }
                KeyCode::Down => {
                    if *selected_index + 1 < total_items {
                        *selected_index += 1;
                    }
                    None
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    // The trailing "add model" row has no provider
                    if refreshing.is_none()
                        && let Some(entry) = models.get(*selected_index)
                    {
                        Some(ModelPickerAction::Refresh(entry.provider_id.clone()))
                    } else {
                        None
                    }
                }
                KeyCode::Enter => {
                    if *selected_index < models.len() {
                        Some(ModelPickerAction::Switch(models[*selected_index].clone()))
                    } else {
                        Some(ModelPickerAction::AddModel)
                    }
                }
                _ => None,
            }
        };

        match action {
            Some(ModelPickerAction::Close) => {
                self.state = AppState::Chat;
            }
            Some(ModelPickerAction::Switch(entry)) => {
                self.switch_model(&entry)?;
            }
            // Same form as first-run setup, minus the welcome page: pick provider (incl. the
            // unconfigured built-in ones), key, fetch /models, tick the models to add
            Some(ModelPickerAction::AddModel) => {
                self.state = AppState::Setup(SetupForm::new(&self.config, true));
            }
            Some(ModelPickerAction::Refresh(provider_id)) => {
                self.refresh_models_in_picker(provider_id)?;
            }
            None => {}
        }
        Ok(())
    }

    /// Kick off the background `/models` fetch for one provider (R key in the model picker).
    /// The result comes back as [`AppEvent::ModelsRefreshed`]; it opens the multi-select
    /// dialog of the setup form.
    fn refresh_models_in_picker(&mut self, provider_id: String) -> Result<()> {
        let AppState::ModelPicker {
            refreshing,
            refresh_note,
            ..
        } = &mut self.state
        else {
            return Ok(());
        };
        if refreshing.is_some() {
            // One fetch at a time
            return Ok(());
        }
        match model_discovery::check_refreshable(&self.config, &provider_id) {
            Ok((api_key, base_url)) => {
                *refresh_note = None;
                *refreshing = Some(provider_id.clone());
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
            }
            Err(e) => {
                *refresh_note = Some((false, e.to_string()));
            }
        }
        Ok(())
    }

    /// Apply the background fetch result: on success open the setup form's multi-select step
    /// (append mode: picked models are added, the active model stays untouched); a failure
    /// shows up as a note in the picker.
    fn handle_models_refreshed(
        &mut self,
        provider_id: &str,
        result: Result<Vec<RemoteModel>, String>,
    ) -> Result<()> {
        // Finish the spinner and decide where to go (state borrow ends here)
        let fetched = {
            let AppState::ModelPicker {
                refreshing,
                refresh_note,
                ..
            } = &mut self.state
            else {
                return Ok(());
            };
            // Only the fetch we are waiting for may finish the spinner
            if refreshing.as_deref() != Some(provider_id) {
                return Ok(());
            }
            *refreshing = None;
            match result {
                Err(e) => {
                    *refresh_note = Some((false, e));
                    None
                }
                Ok(remote_models) if remote_models.is_empty() => {
                    *refresh_note = Some((false, "服务商未返回任何模型".to_string()));
                    None
                }
                Ok(remote_models) => {
                    *refresh_note = None;
                    Some(remote_models)
                }
            }
        };
        let Some(remote_models) = fetched else {
            return Ok(());
        };

        let mut form = SetupForm::new(&self.config, true);
        if let Some(p) = form.provider_options.iter().find(|p| p.id == provider_id) {
            form.provider_kind = p.kind;
            form.base_url = p.base_url.clone();
        } else {
            form.provider_kind = ProviderKind::Configured;
            form.base_url = self
                .config
                .provider_base_url(provider_id)
                .unwrap_or_default();
        }
        form.provider_id = provider_id.to_string();
        form.remote_models = remote_models;
        form.existing_ids = self
            .config
            .providers
            .get(provider_id)
            .map(|p| p.models.keys().cloned().collect())
            .unwrap_or_default();
        form.step = SetupStep::SelectModel;
        self.state = AppState::Setup(form);
        Ok(())
    }
}
