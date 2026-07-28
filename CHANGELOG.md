# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0](https://github.com/dongdong306/hailux/compare/v0.1.1...v0.2.0) (2026-07-28)


### Features

* 自定义 changelog 格式化，按 Improvements/Bugfixes 等分类生成 release notes ([6a5ee95](https://github.com/dongdong306/hailux/commit/6a5ee952773ccbee97f4aea00c6f664dc8336bc1))


### Bug Fixes

* bump-minor-pre-major 设为 false，feat 在 1.0 前只 bump patch ([3d9892e](https://github.com/dongdong306/hailux/commit/3d9892e65b87f536e1346d98ef2ff2335524c134))

## [0.1.1](https://github.com/dongdong306/hailux/compare/v0.1.0...v0.1.1) (2026-07-28)


### Bug Fixes

* 修正 AGENTS.md 中 agents_md.rs 的模块名拼写 ([0e14735](https://github.com/dongdong306/hailux/commit/0e147351d66bddd4a1df7b0f2aa06a17f0ebcc59))

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
