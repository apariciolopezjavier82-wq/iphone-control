# 自动解锁

默认关闭。开启后，手机锁屏时，MCP 操作前会先用你保存的锁屏密码解锁手机。截图、读元素、`iphone_wait_for` 这些只读工具也一样。WDA 不可用时不会自动解锁。

::: danger 先想清楚风险
密码**只留在守护进程内存里**，不写入数据目录，重启后就没了。能调用这个 MCP 的人，以及守护进程还在时的同一用户和 root，仍然能解锁手机。只在你完全掌控的主机上开启，最好用一部专门用来自动化的手机。
:::

## 开启

```bash
docker exec -it iphone-control iphone-control passcode set
```

守护进程必须已经在运行。密码留给它正在控制的那台设备；要给别的设备设，加 `--device <UDID 或设备名>`。只支持 4 到 32 位的纯数字密码，不写入磁盘。关闭：

```bash
docker exec -it iphone-control iphone-control passcode clear           # 正在控制的那台；别的设备加 --device
docker exec -it iphone-control iphone-control passcode clear --all     # 所有设备
```

## 多台设备

密码**按设备留在内存里**，一台的密码不会输进另一台；没设密码的设备锁屏时要人工解锁。`devices` 命令（或 `iphone_list_devices` 工具）会标出这一次进程里哪台设过。

旧版本把明文写在 `secrets/passcode` 或 `secrets/passcodes/<UDID>`。这些文件不再读取、也不再改名，一直留到你用 `passcode clear`（一台）或 `passcode clear --all` 删除。命令只打印路径，不打印密码。要重新开启，在守护进程运行时再执行 `passcode set`。

## 保护规则

- 每按一位都核对密码栏的位数，对不上立即停止（没输完不算输错）；只有第一位没生效时会重来，最多 3 轮。
- 整串输完手机仍然锁着，就**立即停用**自动解锁并推送通知，不会第二次再试，因此不会累计输错触发抹掉数据。
- 从第 2 位起先写停用标记（数据目录的 `logs/UNLOCK-DISABLED`），确认解开或确认没输满才撤掉。所以输入中途出错、进程被杀，也会保持停用。
- 停用标记是**全局**的：一台失败，所有设备的自动解锁都停用。
- 手机重启后第一次解锁（BFU）必须人工完成，自动解锁不处理。
- 手机横放充电时会进入**待机显示**，那个界面调不出密码键盘，自动解锁会失败。开启自动解锁时，在「设置 → 待机显示」里把它关掉。

## 停用后恢复

先人工解锁手机。如果是密码错了（例如你改了锁屏密码），先更新密码，再恢复：

```bash
docker exec -it iphone-control iphone-control passcode set
docker exec -it iphone-control iphone-control unlock-reset
```

如果是输入中途出错、进程被杀，密码本身可能没错，确认无误后只运行 `unlock-reset` 就行。`unlock-reset` 不会核对密码，`passcode set`、`passcode clear` 也不会解除停用。不要靠删除 `logs/` 来恢复，那会撤掉这层保护。

::: tip 不想用自动解锁
可以在手机上把「设置 → 显示与亮度 → 自动锁定」设为「永不」，手机就不会自己锁屏。
:::
