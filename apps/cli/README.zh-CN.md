# TestHarbor CLI

[English](./README.md)

`testharbor` 与桌面端复用同一套 application 流程和本地数据目录，也使用相同的状态迁移、Guard、worktree、Trust 决策、执行证据和交付校验。

## 构建

```bash
cargo build -p testharbor-process-launcher -p testharbor-cli
./target/debug/testharbor --help
```

本地 release 构建需要把 `testharbor` 和 `testharbor-process-launcher` 放在同一目录：

```bash
cargo build --release -p testharbor-process-launcher -p testharbor-cli
./target/release/testharbor --help
```

CLI 默认使用桌面端数据目录。CI 或临时测试应使用隔离目录：

```bash
testharbor --data-dir /tmp/testharbor-data info
```

自动化消费命令输出时可追加 `--json`。

## 创建任务

交互式初始化：

```bash
testharbor init /path/to/repository
testharbor init /path/to/repository --generate
```

显式流程：

```bash
testharbor projects open /path/to/repository
testharbor runs create --project-id <project-id>

testharbor generate <run-id> \
  --prompt '验证服务发布流程' \
  --criteria '发布后可以查询版本详情' \
  --environment '本地服务已启动；测试账号：...；MySQL 和 Redis 使用仓库配置。' \
  --follow

testharbor review show <run-id>
testharbor review approve <run-id>
testharbor execute start <run-id> --wait
testharbor result accept-pass <run-id>
testharbor apply guard <run-id>
testharbor apply export <run-id> --complete
```

打开项目不会再创建占位 Run，必须显式使用 `runs create`。

## Provider 与运行时输入

生成时可以通过 `--agent codex|claude|grok` 选择 Provider，并通过 `--model <cli-model-id>` 选择模型。可用能力和模型由已安装的 CLI 动态发现：

```bash
testharbor models codex
testharbor models claude
testharbor models grok
testharbor config show
testharbor config agents --enable codex --enable claude --enable grok
```

运行时秘密使用 `--var KEY` 从当前进程环境继承。不要把秘密值写进参数、测试目标、附件或 Profile 文件。服务只能使用已确认的结构化 Service Profile，不接受自由文本 startup/shutdown shell 命令。

常规生成入口只保留测试提示词、验收项和可选的环境说明。代码锚点、受影响文件、服务端口、认证流程和测试边界由 TestHarbor/Agent 根据仓库及配置自动发现。旧版的锚点、范围、附件和 Profile 命令仍保留兼容，但不再出现在常规帮助和主流程中。

## 多任务

多个 CLI 进程可以同时操作不同 Run。CLI 与桌面端通过共享 SQLite 数据库协调进程租约，因此遵守相同的全局进程上限。可使用 `runs list`、`execute poll` 和 `execute cancel` 查看或终止任务，不要求始终保持单个终端前台运行。

## 语言

默认语言为英语，语言偏好与桌面端共享并持久化：

```bash
testharbor config language en
testharbor config language zh-CN
testharbor --language en info
```

在机密代码上运行前，请阅读仓库的[安全模型](../../docs/security-model.md)。
