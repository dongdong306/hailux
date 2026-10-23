use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::tui::event::AppEvent;

impl App {
    /// 按范围 + 窗口查询统计快照。
    pub(super) async fn load_stats_data(
        &self,
        work_dir: Option<&str>,
        days: u32,
    ) -> Result<crate::tui::stats_viewer::StatsData> {
        let summary = self.storage.usage_summary(work_dir, days).await?;
        let total = self.storage.usage_summary(work_dir, 0).await?;
        let daily = self.storage.usage_daily(work_dir, days).await?;
        let by_model = self.storage.usage_by_model(work_dir, days).await?;
        let by_project = self.storage.usage_by_project(work_dir, days, 8).await?;
        let recent = self.storage.list_recent_usage(work_dir, days, 50).await?;
        Ok(crate::tui::stats_viewer::StatsData {
            summary,
            total,
            daily,
            by_model,
            by_project,
            recent,
            days,
        })
    }

    /// 按当前 Stats 页面的项目/窗口重新查询（含项目列表）。
    pub(in crate::tui::app) async fn refresh_stats(&mut self) -> Result<()> {
        let (work_dir, days) = {
            let AppState::Stats { data, work_dir, .. } = &self.state else {
                return Ok(());
            };
            (work_dir.clone(), data.days)
        };
        let data = self.load_stats_data(work_dir.as_deref(), days).await?;
        let projects = self.storage.list_work_dirs().await?;
        if let AppState::Stats {
            data: d,
            selected_index,
            projects: ps,
            ..
        } = &mut self.state
        {
            *d = data;
            *ps = projects;
            *selected_index = 0;
        }
        Ok(())
    }

    /// 应用新的项目筛选并重新查询（保持窗口不变）。
    async fn apply_stats_scope(&mut self, work_dir: Option<String>) -> Result<()> {
        let days = {
            let AppState::Stats { data, .. } = &self.state else {
                return Ok(());
            };
            data.days
        };
        let data = self.load_stats_data(work_dir.as_deref(), days).await?;
        if let AppState::Stats {
            data: d,
            selected_index,
            work_dir: w,
            ..
        } = &mut self.state
        {
            *d = data;
            *w = work_dir;
            *selected_index = 0;
        }
        Ok(())
    }

    /// Stats 页面事件：项目选择器打开时优先处理（↑↓/Enter/Esc）；
    /// 否则 ↑↓/PgUp/PgDn 选择明细，←→ 切时间窗口，Tab 全部↔当前项目，p 选择项目，r 刷新。
    pub(in crate::tui::app) async fn handle_stats_event(&mut self, event: AppEvent) -> Result<()> {
        let picker_open = matches!(
            &self.state,
            AppState::Stats {
                picker_index: Some(_),
                ..
            }
        );

        // 鼠标滚轮：步进移动（选择器打开时移动选择器）
        match event {
            AppEvent::ScrollUp => {
                let delta: i64 = 3;
                if picker_open {
                    Self::stats_picker_step(&mut self.state, -delta);
                } else if let AppState::Stats { selected_index, .. } = &mut self.state {
                    *selected_index = (*selected_index as i64 - delta).max(0) as usize;
                }
                return Ok(());
            }
            AppEvent::ScrollDown => {
                let delta: i64 = 3;
                if picker_open {
                    Self::stats_picker_step(&mut self.state, delta);
                } else if let AppState::Stats {
                    data,
                    selected_index,
                    ..
                } = &mut self.state
                    && (*selected_index as i64 + delta) < data.recent.len() as i64
                {
                    *selected_index = (*selected_index as i64 + delta) as usize;
                }
                return Ok(());
            }
            _ => {}
        }

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        // ── 项目选择器 ─────────────────────────────────────
        if picker_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('p') | KeyCode::Tab => {
                    if let AppState::Stats { picker_index, .. } = &mut self.state {
                        *picker_index = None;
                    }
                }
                KeyCode::Up => Self::stats_picker_step(&mut self.state, -1),
                KeyCode::Down => Self::stats_picker_step(&mut self.state, 1),
                KeyCode::PageUp => Self::stats_picker_step(&mut self.state, -10),
                KeyCode::PageDown => Self::stats_picker_step(&mut self.state, 10),
                KeyCode::Home => {
                    if let AppState::Stats { picker_index, .. } = &mut self.state
                        && let Some(i) = picker_index.as_mut()
                    {
                        *i = 0;
                    }
                }
                KeyCode::End => {
                    if let AppState::Stats {
                        picker_index,
                        projects,
                        ..
                    } = &mut self.state
                        && let Some(i) = picker_index.as_mut()
                    {
                        *i = projects.len(); // 最后一项（非“全部项目”）
                    }
                }
                KeyCode::Enter => {
                    let pick = {
                        let AppState::Stats {
                            picker_index,
                            projects,
                            ..
                        } = &self.state
                        else {
                            return Ok(());
                        };
                        match picker_index {
                            Some(0) => None,
                            Some(i) => projects
                                .get(i.saturating_sub(1))
                                .map(|p| p.work_dir.clone()),
                            None => None,
                        }
                    };
                    if let AppState::Stats { picker_index, .. } = &mut self.state {
                        *picker_index = None;
                    }
                    self.apply_stats_scope(pick).await?;
                }
                _ => {}
            }
            return Ok(());
        }

        let (recent_len, days) = {
            let AppState::Stats { data, .. } = &self.state else {
                return Ok(());
            };
            (data.recent.len(), data.days)
        };

        let mut step: Option<i64> = None;
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => step = Some(-1),
            KeyCode::Down => step = Some(1),
            KeyCode::PageUp => step = Some(-10),
            KeyCode::PageDown => step = Some(10),
            // ←→ 切换时间窗口（7 → 30 → 90 → 全部 → 7）
            KeyCode::Left | KeyCode::Right => {
                if let Some(pos) = crate::tui::stats_viewer::STATS_WINDOWS
                    .iter()
                    .position(|d| *d == days)
                {
                    let next = if key.code == KeyCode::Right {
                        (pos + 1) % crate::tui::stats_viewer::STATS_WINDOWS.len()
                    } else {
                        (pos + crate::tui::stats_viewer::STATS_WINDOWS.len() - 1)
                            % crate::tui::stats_viewer::STATS_WINDOWS.len()
                    };
                    let new_days = crate::tui::stats_viewer::STATS_WINDOWS[next];
                    let work_dir = {
                        let AppState::Stats { work_dir, .. } = &self.state else {
                            return Ok(());
                        };
                        work_dir.clone()
                    };
                    let data = self.load_stats_data(work_dir.as_deref(), new_days).await?;
                    if let AppState::Stats {
                        data: d,
                        selected_index,
                        ..
                    } = &mut self.state
                    {
                        *d = data;
                        *selected_index = 0;
                    }
                }
            }
            // Tab 快速切换 全部项目 ↔ 当前项目
            KeyCode::Tab => {
                let is_all = matches!(&self.state, AppState::Stats { work_dir: None, .. });
                let work_dir = if is_all {
                    Some(Self::current_work_dir()?)
                } else {
                    None
                };
                self.apply_stats_scope(work_dir).await?;
            }
            // p 打开项目选择器（定位到当前项目）
            KeyCode::Char('p') => {
                if let AppState::Stats {
                    work_dir,
                    projects,
                    picker_index,
                    ..
                } = &mut self.state
                {
                    let init = match work_dir.as_deref() {
                        None => 0,
                        Some(w) => projects
                            .iter()
                            .position(|p| p.work_dir == w)
                            .map_or(0, |i| i + 1),
                    };
                    *picker_index = Some(init);
                }
            }
            KeyCode::Char('r') => {
                self.refresh_stats().await?;
            }
            _ => {}
        }

        if let Some(delta) = step
            && let AppState::Stats { selected_index, .. } = &mut self.state
        {
            let max = recent_len.saturating_sub(1);
            *selected_index = (*selected_index as i64 + delta).clamp(0, max as i64) as usize;
        }
        Ok(())
    }

    /// 项目选择器选中项步进（clamp 到 [0, projects.len()]）。
    fn stats_picker_step(state: &mut AppState, delta: i64) {
        if let AppState::Stats {
            picker_index: Some(i),
            projects,
            ..
        } = state
        {
            let max = projects.len() as i64;
            *i = ((*i as i64) + delta).clamp(0, max) as usize;
        }
    }
}
