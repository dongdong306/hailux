# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v0.1.0](https://github.com/dongdong306/hailux/releases/tag/v0.1.0) (2026-07-28)

### Added

- 流式对话，支持 DeepSeek 推理过程（reasoning_content）展示
- 内置工具：文件读写、搜索、Bash 执行、网页获取、任务列表、子代理等
- MCP 协议支持（stdio + http 传输）
- 技能系统（SKILL.md，支持全局与项目级）
- 规划模式（只读模式）
- 子代理任务面板
- 会话管理与历史持久化（SQLite）
- 多模型支持（DeepSeek、智谱 AI 及自定义提供商）
- 终端 Markdown 渲染与语法高亮
- 文件提及（@路径）
- 斜杠命令（/sessions、/new、/models、/skills、/mcp、/tasks、/plan、/exit）
