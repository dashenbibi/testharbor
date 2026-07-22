# TestHarbor

[English](./README.md)

TestHarbor 是一个实验阶段、本地优先的仓库测试编排工具。它通过桌面端或 CLI 协调 AI 辅助测试生成、确定性校验、人工评审、受控执行和显式交付。

> TestHarbor 目前属于早期预览版本。请只在可恢复的仓库中使用，逐条审查命令与补丁，并且不要把它的进程控制视为操作系统级安全沙箱。

## 主要能力

- 固化包含 base commit、测试目标、Profile、命令身份和信任范围的不可变 Run Snapshot。
- 在隔离 Git worktree 中生成候选测试。
- 在评审和执行前校验路径与候选内容。
- 将 Agent 评审、确定性检查和人工批准彼此分离。
- 通过受监督的进程启动器执行结构化 Test Profile 和 Service Profile。
- 持久化 Revision、Execution、Decision、日志和 artifact 审计历史。
- 桌面端和 CLI 均可管理多个 Run；进程槽通过共享本地数据库协调，默认全局 OS 进程上限为 2。
- 默认使用英语，同时支持简体中文。

## 当前能力边界

| 范围 | 当前支持 |
| --- | --- |
| 桌面端 | 以 macOS 为首发平台的 Tauri 2 应用 |
| CLI | 与桌面端共享数据目录的本地 `testharbor` 命令 |
| Provider | 通过 capability probe 后启用 Codex CLI、Claude Code CLI、Grok CLI |
| 严格通过决策 | 具有稳定 Inventory 和 intended-test 观察结果的 Go Test JSON |
| 其他结果 | JUnit 和 Generic exit-code 证据可供评审，但不具备同等级严格通过保证 |
| 进程隔离 | Unix 进程组、超时、取消和并发限制；不提供 VM/容器沙箱 |
| 交付 | 人工接受后显式导出，或通过 Guard 后 Apply |

正常运行不会静默切换到 Mock。Mock 只用于测试和显式本地 dogfood。

## 安全与隐私

TestHarbor 会运行用户确认的 Git、Provider CLI、测试工具和服务命令。这些程序继承当前用户的操作系统权限，并可能访问网络。发给 Provider 的内容可能包含仓库派生的测试目标、附件、diff、校验结果和测试证据。

在用于机密代码前，请阅读[安全模型](./docs/security-model.md)和[Provider 数据流](./docs/provider-data-flow.md)。安全漏洞请按 [SECURITY.md](./SECURITY.md) 私下报告。

## 环境要求

- 受支持桌面发布路径使用 macOS
- Rust 1.88 或更高版本
- Node.js 22.12 或更高版本及 npm
- Git
- 使用真实 Provider 时，需要单独安装并登录对应 CLI
- Test Profile 所选择的项目测试工具

## 构建与测试

```bash
cargo test --workspace
cargo check -p testharbor-desktop

cd apps/desktop
npm ci
npm run build
npm run tauri dev
```

桌面安装包会通过 `apps/desktop/scripts/prepare-sidecar.sh` 构建并嵌入当前 target 对应的进程启动器：

```bash
cd apps/desktop
npm run tauri:bundle
```

## CLI 快速开始

```bash
cargo build -p testharbor-process-launcher -p testharbor-cli
./target/debug/testharbor --help

testharbor projects open /path/to/repository
testharbor runs create --project-id <project-id>
testharbor runs list
```

自动化场景可使用 `--json`。完整流程见 [CLI 文档](./apps/cli/README.zh-CN.md)。

## 切换语言

```bash
testharbor config language en
testharbor config language zh-CN
testharbor --language en info
```

语言偏好保存在桌面端与 CLI 共用的本地设置中，也可以从桌面端的系统设置弹窗切换。

## 架构

```text
React UI -> Tauri Commands -> application -> domain / adapters
```

桌面端和 CLI 共用领域状态机与 Guard。更多内容见[架构](./docs/architecture.md)、[测试](./docs/testing.md)、[路线图](./docs/roadmap.md)和[公开发布检查表](./docs/release-checklist.md)。

## 参与贡献

欢迎贡献。请先阅读 [CONTRIBUTING.md](./CONTRIBUTING.md) 并遵守 [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md)。

TestHarbor 使用 [MIT License](./LICENSE)。Provider 和产品名称属于各自权利人；本项目不隶属于 OpenAI、Anthropic、xAI 或其他 Provider 厂商，也未获得其赞助。
