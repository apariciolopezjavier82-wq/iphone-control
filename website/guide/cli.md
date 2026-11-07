# 命令行

所有命令都是 `iphone-control <命令>`。Docker 部署时前面加 `docker exec -it iphone-control`；从源码运行和在 macOS 上直接运行。`--help` 看帮助，`--version` 看版本。

## 全局选项

写在命令前面，例如 `iphone-control --socket /path/ctl.sock mcp`。

| 选项 | 环境变量 | 含义 |
|---|---|---|
| `--data-dir <目录>` | `ICTL_DATA_DIR` | 数据目录，默认值见[配置](./configuration#目录和控制接口) |
| `--socket <路径>` | `ICTL_SOCKET` | 守护进程的控制接口，默认值同上 |

## 运行

| 命令 | 作用 |
|---|---|
| `daemon` | 常驻守护进程。容器的入口；从源码运行时由 systemd 或手动启动，macOS 上在终端里运行 |
| `mcp` | MCP stdio 服务，由 AI 客户端拉起 |
| `mcp-config [--container <容器名>]` | 打印把 MCP 注册到 Claude Code 的命令。在容器里或给了 `--container` 时打印 `docker exec` 的写法，否则直接调用本程序 |

## 安装与签名

| 命令 | 作用 |
|---|---|
| `setup` | 安装向导：配对、开发者模式、登录 Apple ID、签名安装 WDA。可重复运行，只作用于正在控制的那台 |
| `login [邮箱]` | 登录 Apple ID。密码和验证码只在这里输入；不给邮箱就交互输入 |
| `sign` | 立即签名并安装 WDA，同 `resign --force`。会把设备注册到 Apple ID |
| `resign [--force]` | 描述文件剩余不到 `RESIGN_MIN_DAYS` 天才重签；`--force` 立即重签。不带 `--force` 时不会替没装过 WDA 的设备注册 |

## 查看状态

| 命令 | 作用 |
|---|---|
| `status` | 状态、手机、选设备方式、描述文件还剩多久、自动解锁、通知、最近日志 |
| `doctor` | 逐层诊断，每项失败都给出修复办法 |

## 多台设备

见[多台设备](./multi-device)。

| 命令 | 作用 |
|---|---|
| `devices` | 列出 USB 上接着的设备，标出正在控制哪台、有没有设自动解锁密码 |
| `select <UDID\|设备名\|auto>` | 改为控制另一台，重启后还记得；`auto` 改回自动选 |

## 自动解锁

见[自动解锁](./auto-unlock)。`--device` 接 UDID 或设备名，设备要接着；不给就是正在控制的那台。

| 命令 | 作用 |
|---|---|
| `passcode set [--device <设备>]` | 把这台设备的锁屏密码放进守护进程内存并开启自动解锁。守护进程必须在运行，不写入磁盘 |
| `passcode clear [--device <设备>]` | 忘掉这台设备内存里的密码，并删除它的旧明文文件（如果有） |
| `passcode clear --all` | 忘掉全部内存中的密码，并删除旧的明文密码文件 |
| `unlock-reset` | 自动解锁被停用后恢复。它不核对密码，先自己确认密码没错 |

## 其他

| 命令 | 作用 |
|---|---|
| `notify-test [事件]` | 发测试通知：每类事件各一条，或只发一类（`resign`、`session`、`pin`、`pin-recovered`、`unlock`、`offline`、`online`） |
| `wda-bridge [--port <端口>]` | 把 WDA 桥接到 `127.0.0.1`，端口默认 8100。请求要带 0600 令牌文件里的 Bearer 令牌（命令只打印路径）。用完就停 |
| `credentials discard-legacy [--yes]` | 列出封存前留下的明文副本。`--yes` 只删除 `*.legacy-plaintext`，不打印内容 |
| `keyring-check` | 创建、读回并撤销一把临时合成内核密钥。不登录苹果、不碰 USB、不保存秘密。密钥环不可用时以非零状态退出 |

调试用的 `debug` 子命令默认不编译，见[贡献指南](https://github.com/Derrors/iphone-control/blob/main/CONTRIBUTING.md#testing-on-a-real-phone)。

## 使用说明

- **要守护进程在运行的：** `setup`、`status`、`doctor`、`sign`、`resign`、`devices`、`select`、`unlock-reset`，以及 MCP 工具调用。
- **要在终端里交互运行的：** `setup`、`login`、`passcode set`，Docker 部署要带 `-it`。
- **环境变量：** 命令行读的是当前 shell 的环境。从源码运行和在 macOS 上，`~/.config/iphone-control/env` 只有守护进程会加载；用 `login`、`setup`（如果配了 `ICTL_ANISETTE_URL`）、`notify-test` 前先运行 `set -a; . ~/.config/iphone-control/env; set +a`。
