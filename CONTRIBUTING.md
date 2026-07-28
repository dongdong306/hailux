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

