# Docker 部署

推荐的部署方式。镜像里是一个 Rust 二进制加 Debian 的 usbmuxd，容器不用 `privileged`。

## 安装

三种方式任选其一，跑起来的容器完全一样。

### 只写一个 compose 文件

不用克隆仓库，最省事。新建一个目录（例如 `~/iphone-control`），在里面写 `compose.yaml`：

```yaml
services:
  iphone:
    image: ghcr.io/derrors/iphone-control:latest   # 或固定版本，例如 :0.1.4
    container_name: iphone-control
    # 只开放 USB 字符设备，不需要 privileged
    device_cgroup_rules:
      - "c 189:* rmw"
    init: true
    stop_grace_period: 30s
    security_opt:
      - no-new-privileges:true
    # 默认用独立网络。只有代理在宿主机 127.0.0.1 上时才取消下面这行的注释：
    # network_mode: host
    restart: unless-stopped
    environment:
      # 以下都可选，按需取消注释，完整列表见「配置」
      # HTTP_PROXY: http://127.0.0.1:7890
      # HTTPS_PROXY: http://127.0.0.1:7890
      NO_PROXY: localhost,127.0.0.1,::1
      # BARK_SERVER: https://api.day.app
      # BARK_KEY: 你的key
      # TG_BOT_TOKEN: "123456:ABC..."
      # TG_CHAT_ID: "你的chat id"
    volumes:
      - /dev/bus/usb:/dev/bus/usb
      - ./data:/var/lib/iphone-control
```

然后在这个目录里：

```bash
docker compose up -d
```

`./data` 是相对 `compose.yaml` 所在目录的，里面有配对记录和 Apple ID 会话，**要备份**。环境变量也可以不写在 `environment:` 里，改成放进同目录的 `.env`、再加一段 `env_file: .env`；`.env` 里有通知密钥时，别提交到 git。各变量的含义见[配置](../guide/configuration)。

### 自己构建

```bash
git clone https://github.com/Derrors/iphone-control && cd iphone-control
cp .env.example .env
docker compose up -d --build
```

### 用预构建镜像

镜像在 `ghcr.io/derrors/iphone-control`（amd64、arm64）。在 `.env` 里设：

```ini
IMAGE=ghcr.io/derrors/iphone-control:latest   # 或固定版本，例如 :0.1.4
```

然后：

```bash
docker compose pull && docker compose up -d --no-build
```

装好后运行一次[安装向导](../guide/getting-started)。

## compose 做了什么

- 只开放 USB 字符设备（`device_cgroup_rules: "c 189:* rmw"`），`/dev/bus/usb` 绑定挂载，拔插后新的设备节点在容器里也看得到。
- 默认使用 Compose 的独立网络，并设置 `no-new-privileges`。只有代理在宿主机回环地址上时才打开 `network_mode: host`。容器仍然是 root，仍然绑定整条 USB 总线。
- `./data` 挂到容器的 `/var/lib/iphone-control`，删掉重建容器也不丢配对和登录。
- `init: true`、`stop_grace_period: 30s`：停止时先关掉手机上的会话，避免留下「配对的电脑」图标。

## 升级

升级前先看[更新日志](https://github.com/Derrors/iphone-control/blob/main/CHANGELOG.zh-CN.md)里的升级注意事项。

```bash
docker compose pull && docker compose up -d             # 只写 compose 文件
# 或
git pull
docker compose up -d --build                          # 自己构建
# 或
docker compose pull && docker compose up -d --no-build  # 预构建镜像
```

镜像标签写的是固定版本时，先把 `compose.yaml` 里的版本号改成新版本再 `pull`。

数据在 `./data`，升级不受影响。容器重启后，AI 客户端里的 MCP 连接会断开，重新连接即可（Claude Code 里运行 `/mcp`）。

::: tip 重启后先等就绪
重启后约 10 秒 WDA 就绪。短时间内连续重启多次，手机可能再次弹出「为 XCTest 输入 iPhone 密码」，输入锁屏密码即可。
:::

## 常用命令

```bash
docker exec iphone-control iphone-control status    # 状态
docker exec iphone-control iphone-control doctor    # 诊断
docker logs iphone-control                          # 日志
docker compose down                                 # 停止（数据保留）
```
