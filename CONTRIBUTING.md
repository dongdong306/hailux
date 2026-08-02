# 贡献指南

感谢你对 hailux 的兴趣！欢迎通过以下方式参与贡献。

## 开发环境

- **Rust 1.96.0+**（Edition 2024）
- 推荐使用 RustRover / VS Code 等编辑器

## 快速开始

```sh
git clone https://github.com/dongdong306/hailux.git
cd hailux
cargo build
cargo run
```

## 开发流程

1. Fork 仓库并创建功能分支：
   ```sh
   git checkout -b feature/your-feature
   ```
2. 编写代码，确保通过所有检查：
   ```sh
   cargo fmt          # 格式化
   cargo clippy       # 代码检查（无警告）
   cargo test         # 运行测试
   cargo build        # 确认编译通过
   ```
3. 提交代码，遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范：
   ```
   feat: 新增 xxx 功能
   fix: 修复 xxx 问题
   refactor: 重构 xxx
   docs: 更新文档
   ```
4. 推送分支并创建 Pull Request。

## 代码规范

- 运行 `cargo fmt` 保持代码格式统一
- 运行 `cargo clippy` 确保无警告
- 新功能需附带测试
- 公共 API 需添加文档注释

## 提交信息规范

使用中文或英文均可，格式遵循 Conventional Commits：

| 前缀 | 用途 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `refactor` | 重构（不改变功能） |
| `docs` | 文档变更 |
| `test` | 测试相关 |
| `chore` | 构建、依赖等杂项 |

## 报告问题

- 提交 [Issue](https://github.com/dongdong306/hailux/issues) 描述 Bug 或提出功能建议
- 请使用 Issue 模板，提供尽可能详细的信息

## 发布流程（release-please）

本项目通过 [release-please](https://github.com/googleapis/release-please) 自动生成 release PR 与 GitHub Release，配置见：

- `.github/release-please-config.json` — 分区映射（`changelog-sections`）等配置
- `.release-please-manifest.json` — 各包当前版本基线
- `.github/workflows/release-please.yml` — 触发工作流

**日常无需手动修改 manifest**：合并 release PR 时，release-please 会自动把新版本写回 `.release-please-manifest.json` 并打 tag，版本基线始终自动推进。

需要手动修正 manifest 的常见场景：

- 首次从 simple 模式（`release-type`）切换到 manifest 模式时，manifest 缺失或为空会导致 release-please 从 `0.1.0` 重新开始——此时需把 manifest 钉为当前已发布的最高 tag 版本
- 误合并/回滚了 release PR，需要重新设定基线

注意：工作流中**不要同时设置 `release-type` 与 `config-file`**，否则 action 会走 simple 模式并忽略配置文件（`changelog-sections` 等自定义分区不生效）。

