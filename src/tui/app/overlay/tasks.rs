use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::tui::event::AppEvent;
use crate::tui::tasks_viewer::{TaskEntry, TaskRunStatus};

impl App {
    /// 合并内存 task_records 与数据库 subsession 记录，构建 TaskEntry 列表。
    pub(super) async fn build_task_entries(&self) -> Result<Vec<TaskEntry>> {
        let session_id = match &self.current_session_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };

        let subsessions = self.storage.list_subsessions(session_id).await?;

        // 以 task_records 为主，匹配 subsession
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

        // 补充存在于数据库但不在内存 task_records 中的 subsession（如历史会话恢复后）
        for sub in &subsessions {
            let already = entries
                .iter()
                .any(|e| e.record.as_ref().is_some_and(|r| r.session_id == sub.id));
            if !already {
                // 从 subsession 的 title 解析 subagent 名称和 description
                // title 格式: "subagent_name|description" 或仅 "subagent_name"
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

    /// Tasks 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
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

    /// Task 详情面板的事件处理：↑/↓/PgUp/PgDn 滚动，Esc 返回列表。
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

        // 鼠标滚轮事件
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
