# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.1](https://github.com/dongdong306/hailux/compare/v0.8.0...v0.8.1) (2026-08-18)


### Bugfixes

* **bash:** return on process exit instead of waiting for pipe EOF ([999a0d1](https://github.com/dongdong306/hailux/commit/999a0d1ebee766d04ac80b3d2fb7f2a721610e6e))
* **session:** clear stale in-memory agent context on session create/switch/delete ([87aef17](https://github.com/dongdong306/hailux/commit/87aef17d89e6caba31335e08e7c929a70817a1d4))

## [0.8.0](https://github.com/dongdong306/hailux/compare/v0.7.0...v0.8.0) (2026-08-16)


### Improvements

* add GLM-5.3 model to Zhipu provider ([cba6236](https://github.com/dongdong306/hailux/commit/cba6236d699901111ca5c956a168875975cfa86d))
* add Web UI mode with shared core event architecture ([0d951dd](https://github.com/dongdong306/hailux/commit/0d951dd2dc0a37eaee7ec69710470fddc01b8232))
* enhance ask_user experience in web UI and align TUI behavior ([7ced47c](https://github.com/dongdong306/hailux/commit/7ced47c7e571c0b43dc98d0354ea13a63394ec31))
* group consecutive tool calls and render todo_write as todo card in web UI ([1cd48c1](https://github.com/dongdong306/hailux/commit/1cd48c18222963680e0b5edc2ce7fe9c660a1412))
* list skill directory files in skill tool output ([3b19d1f](https://github.com/dongdong306/hailux/commit/3b19d1f929377f9184d00817b5d118a0ee753e5d))
* show "Load skill" title for skill tool calls in web UI ([2a077a7](https://github.com/dongdong306/hailux/commit/2a077a7bc8497b5d7d56e56d38be54d703726cc1))


### Bugfixes

* **ci:** build web frontend before release binaries ([c711cea](https://github.com/dongdong306/hailux/commit/c711ceab68061d15d3cea99fbf5cc45e99242248))
* correct tool result summary parsing to match actual output ([f9d06ae](https://github.com/dongdong306/hailux/commit/f9d06ae684c5174ca91a2fc1cde9cfeced7dfe7a))
* gate OnceLock import behind windows cfg to fix linux clippy ([6458aad](https://github.com/dongdong306/hailux/commit/6458aad15627dfa783d93e0e871bb6951587a1e2))
* merge predefined models into configured providers' model list ([bffaa4e](https://github.com/dongdong306/hailux/commit/bffaa4eaadfccb42756db688f961662659487350))
* resolve mojibake in bash tool output on Chinese Windows ([bcc8536](https://github.com/dongdong306/hailux/commit/bcc8536e671ba3cca366510d4e75dfe89e70f8cb))
* **web:** reuse empty latest session instead of always creating a new one ([d1ddc8f](https://github.com/dongdong306/hailux/commit/d1ddc8f2d366f4ef7ed4336b81c686d4d348d4f6))


### Performance

* precompute diff hunks for edit/write display data ([5447a5a](https://github.com/dongdong306/hailux/commit/5447a5a774f22d251b547309bd43737011cb7a23))


### Refactor

* remove redundant plan mode badge in web chat input ([6565cbe](https://github.com/dongdong306/hailux/commit/6565cbe6bd50685805f9786bd09f05ea24ff8214))


### Documentation

* add English README and move Chinese to README.zh-CN.md ([432c615](https://github.com/dongdong306/hailux/commit/432c6151dd06c16cbe8cb146bcf093a5529ee6f9))
* update AGENTS.md to reflect web UI architecture and current workflows ([e0c4cea](https://github.com/dongdong306/hailux/commit/e0c4cea4b9633b202521d59101e3375f2f12f327))

## [0.7.0](https://github.com/dongdong306/hailux/compare/v0.6.0...v0.7.0) (2026-08-13)


### Improvements

* add --resume flag to restore latest session on startup ([852b0f7](https://github.com/dongdong306/hailux/commit/852b0f7300d1b54c756aca40e842d51872eeb317))
* add self-update via GitHub releases with SHA256 verification ([dcde994](https://github.com/dongdong306/hailux/commit/dcde9944353601b24491cf975f7137bf9f9b101b))


### Bugfixes

* keep scroll position stable when new content is appended during manual scroll ([d9b9ec5](https://github.com/dongdong306/hailux/commit/d9b9ec588a724a6dacdc3b7347a1d3bfd99653d9))


### Refactor

* remove dead code and unused metadata fields across codebase ([1f4949f](https://github.com/dongdong306/hailux/commit/1f4949f124321ef66106eaf50ae4111587f5ddde))
* simplify paste detection and batch input events for smoother rendering ([5d8fbf9](https://github.com/dongdong306/hailux/commit/5d8fbf910ff93b65bb482e01c5a258a172c8739f))

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
