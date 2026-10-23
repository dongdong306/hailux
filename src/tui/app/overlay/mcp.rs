use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::mcp::{McpConnection, McpToolBackend};
use crate::tui::event::AppEvent;

impl App {
    /// MCP 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
    pub(in crate::tui::app) fn handle_mcp_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::Mcp { selected_index } = &mut self.state else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.mcp_servers.len();
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 && self.mcp_servers[*selected_index].connected => {
                let idx = *selected_index;
                self.state = AppState::McpDetail {
                    server_index: idx,
                    selected_index: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 详情面板的事件处理：↑/↓ 移动选中，Esc 返回列表。
    pub(in crate::tui::app) fn handle_mcp_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::McpDetail {
            server_index,
            selected_index,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
                return Ok(());
            }
        };

        let tools_len = server.tools.len();
        let resources_len = server.resources.len();
        let prompts_len = server.prompts.len();
        let total = tools_len + resources_len + prompts_len;

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 => {
                let si = *server_index;
                let ii = *selected_index;
                self.state = AppState::McpItemDetail {
                    server_index: si,
                    item_index: ii,
                    scroll_offset: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 单项详情面板的事件处理：Esc 返回列表。
    pub(in crate::tui::app) fn handle_mcp_item_detail_event(
        &mut self,
        event: AppEvent,
    ) -> Result<()> {
        let AppState::McpItemDetail {
            server_index,
            item_index,
            scroll_offset,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
                return Ok(());
            }
        };

        let total = server.tools.len() + server.resources.len() + server.prompts.len();

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
            }
            KeyCode::Up => {
                if *item_index > 0 {
                    *item_index -= 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                if total > 0 && *item_index + 1 < total {
                    *item_index += 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::PageUp => {
                *scroll_offset = scroll_offset.saturating_sub(10);
            }
            KeyCode::PageDown => {
                *scroll_offset = scroll_offset.saturating_add(10);
            }
            _ => {}
        }
        Ok(())
    }

    /// 处理后台 MCP 连接完成事件：更新 UI 状态，注册工具给 agent。
    pub(in crate::tui::app) async fn handle_mcp_ready(
        &mut self,
        connections: Vec<McpConnection>,
    ) -> Result<()> {
        self.mcp_servers = connections.iter().map(|c| c.status.clone()).collect();
        let mut backends = self
            .shared
            .mcp_backends
            .lock()
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        backends.clear();
        for conn in &connections {
            if let Some(backend) = &conn.backend {
                let server_name = conn.status.name.clone();
                let backend_arc = backend.clone();
                let tools = conn.tools.clone();
                backends.push(McpToolBackend {
                    server_name,
                    backend: backend_arc,
                    tools,
                });
                for tool in &conn.tools {
                    self.agent.register_tool(Box::new(crate::mcp::McpTool::new(
                        &conn.status.name,
                        tool,
                        backend.clone(),
                    )));
                }
            }
        }
        Ok(())
    }
}
