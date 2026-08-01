# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0](https://github.com/dongdong306/hailux/compare/v0.2.0...v0.3.0) (2026-08-01)


### Features

* TUI redesign, /init command, and visual enhancements ([1860eac](https://github.com/dongdong306/hailux/commit/1860eacde76c396d93a8132a44ac49d504c8539f))
* TUI redesign, /init command, and visual enhancements ([#8](https://github.com/dongdong306/hailux/issues/8)) ([1860eac](https://github.com/dongdong306/hailux/commit/1860eacde76c396d93a8132a44ac49d504c8539f))

## [0.2.0](https://github.com/dongdong306/hailux/compare/v0.1.1...v0.2.0) (2026-07-30)


### Features

* 实现上下文压缩功能，支持自动/手动压缩对话历史 ([#5](https://github.com/dongdong306/hailux/issues/5)) ([e97e433](https://github.com/dongdong306/hailux/commit/e97e4336d59ec7e7d12a988b1f2f65157d1ec9a4))
* 自定义 changelog 格式化，按 Improvements/Bugfixes 等分类生成 release notes ([bb758bd](https://github.com/dongdong306/hailux/commit/bb758bdd04166602a478d16b39da1e0830e2dcaa))

## [0.1.1](https://github.com/dongdong306/hailux/compare/v0.1.0...v0.1.1) (2026-07-28)


### Bug Fixes

* 修正 AGENTS.md 中 agents_md.rs 的模块名拼写 ([0e14735](https://github.com/dongdong306/hailux/commit/0e147351d66bddd4a1df7b0f2aa06a17f0ebcc59))

## [0.1.0](https://github.com/dongdong306/hailux/releases/tag/v0.1.0) (2026-07-28)

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
