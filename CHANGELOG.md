# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0](https://github.com/dongdong306/hailux/compare/v0.5.0...v0.6.0) (2026-08-08)


### Improvements

* show compact duration and update token usage after context compaction ([78ec625](https://github.com/dongdong306/hailux/commit/78ec625de26727334368ac57b09f1e93d0e20669))
* support [@folder](https://github.com/folder) mentions in path picker with lazy cache ([cc89118](https://github.com/dongdong306/hailux/commit/cc891180e939b8868c98c000bb548fa4bf05ad59))
* truncate oversized bash output and glob results ([a8748f0](https://github.com/dongdong306/hailux/commit/a8748f0ee3b312b4e5530a519acac3db82b0763a))


### Refactor

* extract ancestor AGENTS.md discovery into dedicated function ([33cd9d6](https://github.com/dongdong306/hailux/commit/33cd9d6df70321e44050d7fe4c0fb2d96427bed1))
* remove mutex wrapping in streaming loop and tidy constants ([49f0218](https://github.com/dongdong306/hailux/commit/49f0218ef441992e8280cbe1a9079c2ce9bc49e0))
* stop persisting system prompt to database, inject at runtime ([d2f3f51](https://github.com/dongdong306/hailux/commit/d2f3f51814301525e57b8c00607fef3c21b40c05))

## [0.5.0](https://github.com/dongdong306/hailux/compare/v0.4.0...v0.5.0) (2026-08-05)


### Improvements

* add configurable permission system with YOLO mode ([5bb8d59](https://github.com/dongdong306/hailux/commit/5bb8d59472150cbc8a377b5f019fa0a018fbdfbb))
* enforce read-only bash commands in plan mode ([cc69fc7](https://github.com/dongdong306/hailux/commit/cc69fc7eeb255c44689b7b6a752f98dbe5de99df))


### Bugfixes

* exclude popup wait time from response timing stats ([9e1a7e9](https://github.com/dongdong306/hailux/commit/9e1a7e924bb57f6829d4ade59cbd1855ad5297fc))
* resolve Windows short-name path mismatch in permission tests ([e305e35](https://github.com/dongdong306/hailux/commit/e305e35ac9c24ac48a597a3a0c96c71f3090845b))


### Refactor

* broaden system prompt scope and add confirm-before-act workflow ([8ce04a5](https://github.com/dongdong306/hailux/commit/8ce04a585afccb50b65dbae3adeb654951d6bbb1))

## [0.4.0](https://github.com/dongdong306/hailux/compare/v0.3.0...v0.4.0) (2026-08-02)


### Improvements

* add CLI non-interactive mode and modularize app.rs ([8127c6c](https://github.com/dongdong306/hailux/commit/8127c6c1d01eed2aa9191b62cefee5b5a667f7b5))


### Bugfixes

* gate edit/write/bash tools in plan mode ([8fc8f26](https://github.com/dongdong306/hailux/commit/8fc8f26814c71d13dcd7d1dbaaeab6a1e33ea2d9))
* pin release-please manifest to current version 0.3.0 ([abcf1b8](https://github.com/dongdong306/hailux/commit/abcf1b8412d5392578f78279b7d45298bb5fabdc))


### Performance

* store messages as Vec&lt;Arc&lt;Message&gt;&gt; to eliminate deep copies ([0f66037](https://github.com/dongdong306/hailux/commit/0f66037fd1d3d5c7410db272c8026d0a37eec62c))


### CI

* enable manifest-based release-please config ([8b8290b](https://github.com/dongdong306/hailux/commit/8b8290b5a9ca999ccea1a446afe2a756e57234a9))


### Refactor

* allow bash in plan mode and strengthen tool-guidance prompts ([5703c8e](https://github.com/dongdong306/hailux/commit/5703c8e00a8c848823ad0a9d9c56cbda851c7981))
* extract cancellation poll interval into named constant ([8ef8293](https://github.com/dongdong306/hailux/commit/8ef82935e9d1c76e155488a5098164decab38153))
* make tool execution fully async and drop sync execute path ([c9b7c02](https://github.com/dongdong306/hailux/commit/c9b7c02fb1ea937103219f9937cf41aa109b7924))
* redesign TUI input box with rounded border and update message styling ([9725130](https://github.com/dongdong306/hailux/commit/9725130e7e46f3d7779bd631d63cd68d5abe2d0b))
* switch to sqlx versioned migrations and update docs ([6673b96](https://github.com/dongdong306/hailux/commit/6673b96f747851e580ac8fe7447ba8f37d56b120))


### Documentation

* document release-please manifest workflow ([474246e](https://github.com/dongdong306/hailux/commit/474246e2596a65fcab4478009baba2736ffd60a1))

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
