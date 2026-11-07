//! `iphone-control doctor`：逐层诊断，每项失败都给出修复办法。

use std::time::Duration;

use crate::core::{Daemon, DeviceProfile, Inner};

impl Daemon {
    pub(crate) async fn doctor(&self, g: &mut Inner) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut fails = 0;

        out.push("== 设备连接".into());
        match ictl_device::list_devices().await {
            Ok(_) => out.push("✅ usbmuxd 在运行".into()),
            Err(e) => {
                out.push(format!(
                    "❌ 连不上 usbmuxd：{e}\n     → {}",
                    if self.cfg.manage_usbmuxd {
                        "由守护进程自动拉起，看数据目录 logs/usbmuxd.log；宿主机已有 usbmuxd 服务时设 ICTL_MANAGE_USBMUXD=0"
                    } else if cfg!(target_os = "macos") {
                        "macOS 自带 usbmuxd，应一直在运行：sudo launchctl kickstart -k system/com.apple.usbmuxd"
                    } else {
                        "用的是系统的 usbmuxd 服务（插上手机时由 udev 拉起）：确认已安装 usbmuxd、手机已插好"
                    }
                ));
                fails += 1;
            }
        }
        // 判断不了（不是 Linux/macOS）就不报，免得误报「没有设备」
        match ictl_device::host_ios_usb_present().await {
            Some(true) => out.push("✅ USB 上有 iPhone/iPad".into()),
            Some(false) => {
                out.push("❌ USB 上没有 iPhone/iPad\n     → 插好数据线;用 USB-C 转接头的话换一根试试".into());
                fails += 1;
            }
            None => {}
        }
        match (&g.device, &g.info) {
            (Some(d), info) => {
                let i = info.clone().unwrap_or_default();
                out.push(format!("✅ 已连接 {} {} iOS {}", d.udid, i.product_type, i.product_version));
                let major = i.major_version();
                if major > 0 && major < 17 {
                    out.push("⚠️  iOS 低于 17  未在此版本测试过".into());
                } else if major > 0 && major < 27 {
                    out.push("⚠️  iOS 低于 27  HID 后备路线(免签名截图/点击)不可用,只能走 WDA".into());
                }
            }
            // 选定的那台没接、别的接着：usbmuxd 没问题，不是重启它能解决的
            (None, _) if self.pinned(g).is_some() && !g.connected.is_empty() => {
                let p = self.pinned(g).unwrap_or_default();
                let how = if self.cfg.udid.is_some() {
                    "或改 .env 里的 IPHONE_UDID"
                } else {
                    "或 iphone-control select 换成接着的另一台(auto 改回自动选)"
                };
                out.push(format!("❌ 选定的设备 {p} 没连接\n     → 接上它;{how}"));
                fails += 1;
            }
            (None, _) => {
                out.push(
                    if self.cfg.manage_usbmuxd {
                        "❌ 守护进程看不到手机\n     → 等 30 秒,会自动重启 usbmuxd;仍不行就重启容器(或守护进程)"
                    } else if cfg!(target_os = "macos") {
                        "❌ 守护进程看不到手机\n     → 重新插拔数据线;仍不行就 sudo launchctl kickstart -k system/com.apple.usbmuxd"
                    } else {
                        "❌ 守护进程看不到手机\n     → 重新插拔数据线;仍不行就 sudo systemctl restart usbmuxd"
                    }
                    .into(),
                );
                fails += 1;
            }
        }
        out.push(format!("   状态：{}", g.state.describe()));
        if let Some(l) = self.devices_line(g) {
            out.push(format!("   {l}"));
        }

        out.push("== Apple ID 与签名".into());
        match self.account().selected_email().await {
            Ok(_) => out.push("✅ 已登录 Apple ID".into()),
            Err(_) => {
                out.push("❌ 没有登录 Apple ID\n     → iphone-control login".into());
                fails += 1;
            }
        }
        match &g.persisted.wda_bundle {
            Some(b) => {
                out.push(format!("✅ WDA 已签名 {b}"));
                // 接着手机时看手机上装的那份：最近一次签名可能是给别的手机签的
                if g.device.is_some() {
                    self.refresh_device_profile(g).await;
                }
                let whose = match &g.device_profile {
                    DeviceProfile::Expires(_) => "这台手机上的描述文件",
                    DeviceProfile::Unknown(e) => {
                        out.push(format!("⚠️  读不到手机上的描述文件({e}),下面按上次签名的记录"));
                        "描述文件"
                    }
                    _ => "描述文件",
                };
                match g.device_profile.left(&g.persisted) {
                    _ if g.device_profile == DeviceProfile::Missing => {
                        out.push(
                            "❌ 这台手机上没有装当前 Apple ID 签的 WDA\n     → iphone-control setup(会把这台手机注册到 Apple ID 并装上 WDA)"
                                .into(),
                        );
                        fails += 1;
                    }
                    Some(left) if left.is_zero() => {
                        out.push(format!("❌ {whose}已过期\n     → iphone-control resign --force"));
                        fails += 1;
                    }
                    Some(left) if left < Duration::from_secs(48 * 3600) => {
                        out.push(format!("⚠️  {whose}只剩 {} 小时  守护进程会自动重签", left.as_secs() / 3600));
                    }
                    Some(left) => out.push(format!("✅ {whose}有效  还剩 {} 天", left.as_secs() / 86400)),
                    None => out.push("⚠️  不知道描述文件什么时候到期".into()),
                }
            }
            None => {
                out.push("❌ WDA 还没签名安装\n     → iphone-control sign".into());
                fails += 1;
            }
        }
        if let Some(r) = &g.persisted.last_resign {
            out.push(format!("   上次重签：{r}"));
        }

        out.push("== WDA".into());
        match self.wda(g) {
            Some(w) if w.ready().await.unwrap_or(false) => {
                out.push("✅ WDA 就绪（经 usbmuxd 直连；守护进程不监听端口，wda-bridge 运行时除外）".into())
            }
            _ => {
                out.push(format!("❌ WDA 不可用：{}", g.state.describe()));
                fails += 1;
            }
        }

        out.push("== 附加功能".into());
        if self.cfg.notify.any() {
            out.push("✅ 通知已配置".into());
        } else {
            out.push("⚠️  没有配置通知  重签失败、手机离线、需要输密码时不会提醒你".into());
        }
        let unlock = self.unlock_cfg(g, g.device.as_ref().map(|d| d.udid.as_str()));
        let legacy = self.cfg.plaintext_passcode_paths();
        if !legacy.is_empty() {
            let listed = legacy.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
            out.push(format!(
                "⚠️  数据目录里还有明文锁屏密码文件,不会读取也不会自动删除: {listed}\n     \
→ 确认不需要后运行 iphone-control passcode clear --all。要重新开启自动解锁,在守护进程运行时 passcode set(只进内存)"
            ));
        }
        for note in ictl_sign::credential_warnings(&self.cfg.account_dir()) {
            out.push(format!("⚠️  {note}"));
        }
        if unlock.disabled_by_failure() {
            out.push(
                "❌ 自动解锁已停用(之前输错过或输入中途出错)\n     → 核对密码后 iphone-control unlock-reset".into(),
            );
            fails += 1;
        } else if unlock.enabled() {
            out.push("✅ 自动解锁已启用(这台设备)".into());
        } else if g.device.is_some() {
            out.push("⚠️  这台设备没设自动解锁密码  锁屏时需要人工解锁(可选:iphone-control passcode set)".into());
        } else {
            out.push("⚠️  没接设备,看不了自动解锁".into());
        }
        out.push(String::new());
        out.push(if fails == 0 { "全部正常".into() } else { format!("{fails} 项需要处理") });
        out.join("\n")
    }
}
