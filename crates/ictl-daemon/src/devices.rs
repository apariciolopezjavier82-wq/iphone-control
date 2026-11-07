//! 接着多台设备时：列出来、选一台控制。
//!
//! 同一时间只控制一台（一个 WDA 会话、一把设备锁）。选哪台见 [`crate::machine::pick_device`]：
//! 选定了就只认那一台；没选定时正在控制的那台一直用到它断开。切换时拆掉上一台的会话，在新的一台上拉起 WDA

use ictl_device::{Device, DeviceInfo, UsbDevice};

use crate::core::{Daemon, Inner, device_label};

/// 按 UDID 或设备名找设备，返回下标：先比 UDID（不分大小写），再比设备名，最后是设备名包含这段文字且只有一台
pub fn resolve_device(devices: &[DeviceInfo], query: &str) -> Result<usize, String> {
    let q = query.trim();
    let listed = || devices.iter().map(device_label).collect::<Vec<_>>().join("、");
    if devices.is_empty() {
        return Err("USB 上没有接着设备".into());
    }
    if let Some(i) = devices.iter().position(|d| d.udid.eq_ignore_ascii_case(q)) {
        return Ok(i);
    }
    let by_name: Vec<usize> = (0..devices.len()).filter(|&i| devices[i].name == q).collect();
    let hits = if by_name.is_empty() {
        let lq = q.to_lowercase();
        (0..devices.len()).filter(|&i| !lq.is_empty() && devices[i].name.to_lowercase().contains(&lq)).collect()
    } else {
        by_name
    };
    match hits[..] {
        [i] => Ok(i),
        [] => Err(format!("没有接着「{q}」,接着的有 {}", listed())),
        _ => Err(format!(
            "「{q}」对上了不止一台:{};用 UDID 指定",
            hits.iter().map(|&i| device_label(&devices[i])).collect::<Vec<_>>().join("、")
        )),
    }
}

impl Daemon {
    /// 记下这一轮 USB 上接着的设备；新接上的读一次名字、型号
    pub(crate) async fn note_connected(&self, g: &mut Inner, devices: Vec<UsbDevice>) {
        for d in &devices {
            if !g.infos.contains_key(&d.udid)
                && let Ok(i) = Device::from_usb(d).info().await
            {
                g.infos.insert(d.udid.clone(), i);
            }
        }
        g.connected = devices;
    }

    pub(crate) fn list_devices_text(&self, g: &Inner) -> String {
        let infos = self.connected_infos(g);
        if infos.is_empty() {
            return match self.pinned(g) {
                Some(p) => format!("USB 上没有接着设备(选定的是 {p})"),
                None => "USB 上没有接着设备".into(),
            };
        }
        let current = g.device.as_ref().map(|d| d.udid.as_str());
        let mode = match (&self.cfg.udid, self.pinned(g)) {
            (Some(u), _) => format!("IPHONE_UDID 固定为 {u}"),
            (None, Some(p)) => format!("选定了 {p}"),
            (None, None) => "自动选(正在控制的那台断开才换)".into(),
        };
        let mut lines = vec![format!("接着 {} 台,{mode}:", infos.len())];
        for i in &infos {
            let mark = if Some(i.udid.as_str()) == current { "→ 正在控制 " } else { "  " };
            let pin =
                if g.passcodes.contains_key(&i.udid) { "已设(仅本进程内存,重启失效)" } else { "没设" };
            lines.push(format!("{mark}{} 自动解锁密码{pin}", device_label(i)));
        }
        if self.cfg.udid.is_none() && infos.len() > 1 {
            lines.push("切换:iphone_select_device(UDID 或设备名),auto 改回自动选".into());
        }
        lines.join("\n")
    }

    /// 改为控制 `query` 指的那台（UDID、设备名或 `auto`），选定的写进 state.json，重启后还记得
    pub(crate) async fn select_device(&self, g: &mut Inner, query: &str) -> Result<String, String> {
        if let Ok(d) = ictl_device::list_devices().await {
            self.note_connected(g, d).await;
        }
        let q = query.trim();
        if q.eq_ignore_ascii_case("auto") || q == "自动" {
            if let Some(u) = &self.cfg.udid {
                return Err(format!(
                    "IPHONE_UDID 固定为 {u};要改回自动选,先在 .env 里清掉它再重建容器(或重启守护进程)"
                ));
            }
            g.persisted.selected_udid = None;
            g.persisted.save(&self.cfg.state_file());
            return Ok(match g.info.as_ref().filter(|_| g.device.is_some()) {
                Some(i) => format!("已改回自动选;正在控制 {},它断开时换成别的接着的设备", device_label(i)),
                None if g.connected.is_empty() => "已改回自动选;现在没接设备,接上后自动控制它".into(),
                None => "已改回自动选;10 秒内接管接着的设备,先用 iphone_status 确认就绪".into(),
            });
        }
        let infos = self.connected_infos(g);
        let target = infos[resolve_device(&infos, q)?].clone();
        if let Some(u) = &self.cfg.udid
            && !u.eq_ignore_ascii_case(&target.udid)
        {
            return Err(format!(
                "IPHONE_UDID 固定为 {u};要换设备,先改 .env 里的 IPHONE_UDID 再重建容器(或重启守护进程)"
            ));
        }
        if self.cfg.udid.is_none() {
            g.persisted.selected_udid = Some(target.udid.clone());
            g.persisted.save(&self.cfg.state_file());
        }
        let label = device_label(&target);
        if g.device.as_ref().is_some_and(|d| d.udid == target.udid) {
            return Ok(format!("已经在控制 {label},之后固定控制这台"));
        }
        let usb = g.connected.iter().find(|d| d.udid == target.udid).cloned().ok_or("设备刚刚断开了")?;
        // 上一台的锁屏时间不能拿来等这一台
        g.last_lock = None;
        self.online(g, &usb).await;
        let pin = if g.passcodes.contains_key(&target.udid) {
            ""
        } else {
            ";这台没设自动解锁密码,锁屏时要人工解锁(守护进程运行时 iphone-control passcode set,密码只留在内存里)"
        };
        Ok(format!(
            "已切换到 {label},之后固定控制这台。状态:{};WDA 起来要 10~60 秒,先用 iphone_status 确认就绪{pin}",
            g.state.describe()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(udid: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: name.into(),
            product_type: "iPhone16,2".into(),
            product_version: "27.0".into(),
        }
    }

    #[test]
    fn passcode_file_is_per_device_and_stays_in_its_dir() {
        let cfg = crate::Config::from_env(std::path::Path::new("/data"));
        assert_eq!(
            cfg.passcode_file_for("00008020-000925043499002E"),
            cfg.passcodes_dir().join("00008020-000925043499002E")
        );
        let odd = cfg.passcode_file_for("../../etc/x");
        assert_eq!(odd.parent(), Some(cfg.passcodes_dir().as_path()));
        assert_ne!(cfg.passcode_file_for("A"), cfg.legacy_passcode_file());
    }

    #[test]
    fn device_is_found_by_udid_then_name() {
        let ds =
            [dev("00008130-AAAA", "Derrors 的 iPhone"), dev("00008020-BBBB", "Derrors 的 iPad"), dev("C", "iPad 2")];
        assert_eq!(resolve_device(&ds, "00008020-bbbb"), Ok(1));
        assert_eq!(resolve_device(&ds, "Derrors 的 iPhone"), Ok(0));
        assert_eq!(resolve_device(&ds, " iphone "), Ok(0));
        // 「iPad」两台都包含：要用 UDID 指定
        assert!(resolve_device(&ds, "ipad").unwrap_err().contains("不止一台"));
        assert!(resolve_device(&ds, "Mac").unwrap_err().contains("没有接着"));
        assert!(resolve_device(&[], "x").is_err());
        // 名字完全相同的优先于包含
        assert_eq!(resolve_device(&ds, "iPad 2"), Ok(2));
    }
}
