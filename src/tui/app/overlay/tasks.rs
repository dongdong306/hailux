use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::tui::event::AppEvent;
use crate::tui::tasks_viewer::{TaskEntry, TaskRunStatus};

impl App {
    /// Merge in-memory task_records with database subsession records to build the TaskEntry list.
    pub(super) async fn build_task_entries(&self) -> Result<Vec<TaskEntry>> {
        let session_id = match &self.current_session_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };

        let subsessions = self.storage.list_subsessions(session_id).await?;

        // task_records are the source of truth; match subsessions to them
        let mut entries: Vec<TaskEntry> = Vec::new();

        for record in &self.tasks.records {
            let subsession = subsessions
                .iter()
                .find(|s| s.id == record.session_id)
                .cloned();
            entries.push(TaskEntry {
                record: Some(record.clone()),
                subsession,
                subagent_name: record.subagent_name.clone(),
                description: record.description.clone(),
                status: record.status,
            });
        }

        // Add subsessions that exist in the database but not in in-memory task_records
        // (e.g. after restoring a historical session)
        for sub in &subsessions {
            let already = entries
                .iter()
                .any(|e| e.record.as_ref().is_some_and(|r| r.session_id == sub.id));
            if !already {
                // Parse the subagent name and description from the subsession title
                // title format: "subagent_name|description" or just "subagent_name"
                let (name, desc) = if sub.title.contains('|') {
                    let mut parts = sub.title.splitn(2, '|');
                    let n = parts.next().unwrap_or("subagent").to_string();
                    let d = parts.next().unwrap_or("").to_string();
                    (n, d)
                } else if !sub.title.is_empty() {
                    (sub.title.clone(), String::new())
                } else {
                    ("subagent".to_string(), String::new())
                };

                entries.push(TaskEntry {
                    record: None,
                    subsession: Some(sub.clone()),
                    subagent_name: name,
                    description: desc,
                    status: TaskRunStatus::Completed,
                });
            }
        }

        Ok(entries)
    }

    /// Tasks panel event handling: ↑/↓ moves selection, Enter shows details, Esc returns to chat.
    pub(in crate::tui::app) async fn handle_tasks_event(&mut self, event: AppEvent) -> Result<()> {
        let (selected_index, entries_len) = {
            let AppState::Tasks {
                selected_index,
                entries,
            } = &self.state
            else {
                return Ok(());
            };
            (*selected_index, entries.len())
        };

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if selected_index > 0
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if entries_len > 0
                    && selected_index + 1 < entries_len
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if entries_len > 0 => {
                let (entry, idx, entries) = {
                    let AppState::Tasks {
                        entries,
                        selected_index,
                    } = &self.state
                    else {
                        return Ok(());
                    };
                    (
                        entries[*selected_index].clone(),
                        *selected_index,
                        entries.clone(),
                    )
                };

                let session_id = entry
                    .subsession
                    .as_ref()
                    .map(|s| s.id.clone())
                    .or_else(|| entry.record.as_ref().map(|r| r.session_id.clone()));

                let messages = if let Some(ref sid) = session_id {
                    self.storage.load_messages(sid).await.unwrap_or_default()
                } else {
                    Vec::new()
                };

                self.state = AppState::TaskDetail {
                    task_index: idx,
                    scroll_offset: usize::MAX,
                    messages,
                    entries,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// Task detail panel event handling: ↑/↓/PgUp/PgDn scroll, Esc returns to the list.
    pub(in crate::tui::app) async fn handle_task_detail_event(
        &mut self,
        event: AppEvent,
    ) -> Result<()> {
        let task_index = {
            let AppState::TaskDetail { task_index, .. } = &self.state else {
                return Ok(());
            };
            *task_index
        };

        // Mouse wheel events
        match event {
            AppEvent::ScrollUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(3);
                }
                return Ok(());
            }
            AppEvent::ScrollDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(3);
                }
                return Ok(());
            }
            _ => {}
        }

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                let entries = self.build_task_entries().await?;
                let safe_index = task_index.min(entries.len().saturating_sub(1));
                self.state = AppState::Tasks {
                    selected_index: safe_index,
                    entries,
                };
            }
            KeyCode::Up => {
                if task_index > 0 {
                    let new_idx = task_index - 1;
                    let entries = self.build_task_entries().await?;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::Down => {
                let entries = self.build_task_entries().await?;
                let total = entries.len();
                if total > 0 && task_index + 1 < total {
                    let new_idx = task_index + 1;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::PageUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(10);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
