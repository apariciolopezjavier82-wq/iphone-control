# 关键决策记录

格式：背景 → 决定 → 理由 → 代价/何时重新评估。

## D1：基于 idevice，而不是从协议层自己写

- **决定**：设备通讯全部基于 `idevice`。版本用 `[patch.crates-io]` 锁在上游 master 的 git 提交 `89702099`（0.1.68 发版代码的 cryptexd install 会挂住，修复已在 master、未发版）；发版后换回 crates.io。
- **理由**：它已覆盖我们需要的全部能力：usbmuxd、lockdown、CoreDeviceProxy 用户态隧道、RSD、cryptexd 个性化镜像安装、universalhidservice/indigo HID、screencaptureservice、XCTest 启动 WDA、WDA 客户端；MIT；StikDebug、CrossCode 等在生产中使用；Impactor 也依赖它，和签名部分同源。
- **代价**：0.2.0 之前每个小版本都可能有破坏性变更；主要一人维护；`tss` 自述部分支持、TCP 栈自述 "naive"。
- **重新评估**：spike 第 4、5 项不过时（已全部通过，见 [spike-results](dev/archive/spike-results.md)）。

## D2：守护进程独占设备，MCP 是薄客户端

- **决定**：`daemon` 常驻持有隧道、HID 会话、XCTest 会话；`mcp` 通过 Unix socket 调用。
- **理由**：MCP 进程每个会话一个、短命、可能并发；HID 事件要求显示媒体流同时存在；WDA 只能有一个 runner。toolkit 每条 HID 命令重建隧道要 3～5 秒。
- **代价**：多一层协议。

## D3：WDA 不在本机监听端口

- **决定**：用 idevice 的 provider 直连设备端口访问 WDA；外部工具需要时显式开 bridge，只绑 127.0.0.1。
- **理由**：WDA 没有认证；toolkit 早期用 `ios forward` 监听了所有网卡，局域网内任何人都能控制手机。

## D4：WDA 仍然是 Objective-C，在 macOS CI 上编译

- **决定**：不重写 WDA；fork appium/WebDriverAgent，在 GitHub Actions macOS runner 上不签名编译，发布未签名 IPA + sha256。
- **理由**：WDA 是 XCTest bundle，只能用 Xcode 编译，依赖苹果私有/测试框架；Rust 无法替代。
- **代价**：发布流程依赖 macOS runner。
- **现状**：自己编译还没做；镜像构建时暂时下载 Appium 官方 Release 的未签名真机包（v16.15.3，校验 sha256）。

## D5：保留系统 usbmuxd，作为子进程看护

- **决定**：不内置 USB 协议栈。Linux 上使用 Debian 的 `usbmuxd`：容器里作为子进程看护，以普通用户运行时用系统服务。macOS 用系统自带的 usbmuxd，守护进程不启动也不重启它。
- **理由**：idevice 只有 usbmuxd 客户端；自己实现 USB 多路复用收益很小；usbmuxd 是 GPL-3.0，作为独立进程运行不影响本项目许可证。
- **代价**：仍需处理容器无 udev 的热插拔问题（sysfs 兜底 + 重启）。

## D6：签名组件 vendor 而不是 git 依赖

- **决定**：把 Impactor 的 `plume_core`、`plume_store`、`omnisette` 和打过补丁的 apple-codesign vendor 进本仓库。`plume_utils` 不 vendor：需要的 bundle 逻辑改写进 `crates/ictl-sign/src/bundle.rs`（含 `.xctest` 修复）。
- **理由**：Impactor 用 `[patch.crates-io]` 把 apple-codesign 指向本地打过补丁的副本；作为 git 依赖引用时，**依赖方 workspace 的 patch 不会生效**，会拿到未打补丁的 apple-codesign。另外需要修 `.xctest` 漏签，还要去掉许可证有问题的可选依赖（见 THIRD_PARTY.md）。
- **代价**：要定期同步上游；MPL-2.0 文件改动需单独标注。修复同时提 PR 回上游，争取将来改回普通依赖。

## D7：开发者镜像下载由我们负责

- **决定**：`ictl-device` 提供 DDI 下载与缓存（来源可配置，默认与 pymobiledevice3 相同），`install_ddi` 只接收本地目录。
- **理由**：idevice 的 `Cryptex1Assets::load` 只读本地解包目录，不负责下载；苹果文件不能随项目分发。
- **重新评估**：idevice 若加入下载功能。

## D8：自动解锁默认关闭

- **决定**：需要显式开启。密码只放在守护进程内存里，不写入数据目录；重启后要重新 `passcode set`。旧的明文文件不读取、不自动删除。
- **理由**：把锁屏密码放进可备份的文件里，等于把手机的数据保护交给数据目录的每一个副本。0600 挡不住同一用户、root 和备份。内存同样挡不住同一用户和 root，但不会跟着数据目录走。输错累计还可能触发设备抹除。保护规则见 `crates/ictl-unlock/src/lib.rs` 顶部注释。

## D9：所有依赖锁定精确版本

- **决定**：`Cargo.toml` 用 `=x.y.z`，`Cargo.lock` 入库，Docker 基础镜像锁 digest。
- **理由**：toolkit 两次被版本漂移坑过（mcp 2.x 改名；go-ios main 分支命令变化）。idevice 官方也要求锁定。

## D10：与 toolkit 工具接口一致

- **决定**：现在共 17 个 MCP 工具，其中来自 toolkit 的 13 个，工具名、参数、返回文本格式保持与 toolkit 一致。
- **理由**：切换时 agent 侧零改动；toolkit 的回归脚本可直接用于验收。
- **补充（2026-11-01）**：只做向后兼容的扩展，不改已有工具的名字、参数和默认返回。为减少 agent 往返（每次往返是一轮模型推理，
  真机观察约 8～13 秒，远大于工具本身的 0.3～1.5 秒），操作类工具加可选参数 `then`（`elements` / `screenshot`：等界面稳定后
  顺带返回界面），并新增 `iphone_wait_for`（等某段文字出现）和 `iphone_batch`（一次执行最多 20 步，出错即停）。多设备的 `iphone_list_devices`、`iphone_select_device` 见 D11。

## D11：接多台设备时，同一时间只控制一台

- **决定**：一个守护进程可以接多台 iPhone/iPad，但只控制一台（一个 WDA 会话、一把设备锁）。优先级：`IPHONE_UDID` 固定 > 运行中选定的（`select`、`iphone_select_device`，持久化到 `state.json`）> 自动选（正在控制的那台一直用到它断开）。选定的那台没接时就是离线，不静默换到另一台。锁屏密码按设备存；自动解锁的停用标记是全局的。
- **理由**：操作或密码落到另一台手机上后果严重，所以宁可离线也不换台，密码也绝不跨设备使用。同时控制多台要多套会话，每个工具还要加设备参数，和 D10 的接口兼容冲突。停用标记全局是偏保守的选择：一台失败后先让人看一眼。
- **代价**：选定的设备没接时可能触发离线通知；一台自动解锁失败，所有设备都停用；切换后 WDA 要 10～60 秒才起来；自动重签只管正在控制的那台，闲置设备的描述文件会过期，切回后才重签；通知文字不带设备名。
- **重新评估**：出现同时驱动多台的需求时。
