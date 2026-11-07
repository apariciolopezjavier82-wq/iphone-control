# 数据与备份

## 数据目录里有什么

| 路径 | 内容 | 丢了会怎样 |
|---|---|---|
| `account/` | Apple ID 会话、开发签名私钥和证书 | 要重新登录；会生成新证书 |
| `lockdown/` | 配对记录（Docker 部署） | 要在手机上重新点「信任」 |
| `wda/` | 签好的 WebDriverAgent | 自动重签 |
| `ddi/` | 开发者镜像缓存 | 自动重新下载 |
| `secrets/passcodes/` | 各设备的锁屏密码（仅开启自动解锁时） | 要重新 `passcode set` |
| `state.json` | 签好的 WDA 包信息（Team ID、Bundle ID、描述文件到期时间）、选定的设备、最近一台设备的屏幕尺寸、上次重签结果 | 守护进程按「WDA 还没签名」启动：要重新 `sign` 或 `setup`，选过设备的要重新 `select` |
| `logs/` | 日志、自动解锁的停用标记 `UNLOCK-DISABLED` | 日志没了不要紧。但停用标记没了就撤销了这层保护：之前输错过的话，可能再用同一个错误密码输一遍 |

位置：Docker 部署是仓库目录下的 `./data`（root 所有，权限 0700）；从源码运行是 `~/.local/share/iphone-control`，配对记录在系统的 `/var/lib/lockdown`；macOS 上数据目录也是 `~/.local/share/iphone-control`，配对记录由系统保存在 `/var/db/lockdown`（和 Finder 共用）。

::: danger 这些是秘密
数据目录里的配对记录仍然可以不经再次「信任」就连接手机。新保存的 Apple ID 会话是封存的，没有那一次启动的内核密钥环密钥就解不开。旧的明文副本（`*.legacy-plaintext`、`secrets/passcode*`）不会被自动删除，里面仍有秘密。不要提交到 git，不要发给别人，备份也要加密保存。锁屏密码不再写在这里。
:::

## 备份

```bash
docker compose stop
sudo tar czf iphone-control-data.tar.gz data     # 目录是 root 所有，需要 sudo
docker compose start
```

从源码运行：

```bash
systemctl --user stop iphone-control
tar czf iphone-control-data.tar.gz -C ~/.local/share iphone-control
sudo tar czf lockdown.tar.gz /var/lib/lockdown     # 配对记录
systemctl --user start iphone-control
```

macOS：先在守护进程的终端按 Ctrl-C 停掉，再执行上面的第二条 `tar`。配对记录归系统管理，不用备份，换了 Mac 在手机上重新点一次「信任」即可。

## 迁移到新主机

把数据目录（从源码运行时还有 `/var/lib/lockdown`）原样恢复到新主机的同一位置，再启动即可，不需要重新配对、登录。
