//! 守护进程配置：全部来自环境变量（容器里由 .env 注入；从源码运行时由 systemd 的环境文件或 shell 给），都有默认值。
//! 锁屏密码不放这里，也不写入数据目录。通知密钥仍在环境变量里，谁能读进程环境谁就能看到。

use std::path::{Path, PathBuf};
use std::time::Duration;

use ictl_device::ScreenSize;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    /// 固定要控制的手机（`IPHONE_UDID`）；None 时可以在运行中选（见 [`crate::core::Persisted::selected_udid`]）。
    /// 都没选是自动选：正在控制的那台一直用到断开，没有正在控制的才取 usbmuxd 列出的第一台
    pub udid: Option<String>,
    /// 由守护进程拉起并看护 usbmuxd。默认以 root 运行（容器）时为 true；普通用户拉起的 usbmuxd 碰不到 USB，
    /// 改用系统的 usbmuxd 服务。宿主机以 root 运行、又已有 usbmuxd 服务时设 ICTL_MANAGE_USBMUXD=0
    pub manage_usbmuxd: bool,
    pub usbmuxd_bin: String,
    /// 未签名的 WDA：镜像里自带；从源码运行时是数据目录里的 [`Config::wda_unsigned_cache`]，第一次签名前下载
    pub wda_unsigned: PathBuf,
    pub ddi_url: String,
    /// 自己从 Xcode 提取的 DDI Restore 目录；设了就不下载，也从不删除里面的文件
    pub ddi_dir: Option<PathBuf>,
    pub anisette_url: Option<String>,
    /// WDA 从没连上过时，HID 换算坐标用的屏幕尺寸
    pub screen_fallback: Option<ScreenSize>,
    pub resign_min: Duration,
    pub resign_check: Duration,
    pub session_check: Duration,
    /// 0 表示不提醒
    pub offline_alert: Duration,
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Default)]
pub struct NotifyConfig {
    pub tg_bot_token: Option<String>,
    pub tg_chat_id: Option<String>,
    pub bark_server: String,
    pub bark_key: Option<String>,
}

impl NotifyConfig {
    pub fn any(&self) -> bool {
        (self.tg_bot_token.is_some() && self.tg_chat_id.is_some()) || self.bark_key.is_some()
    }
}

/// 读环境变量，去掉首尾空白，空值当作没设（.env 里常留着 `KEY=`）
pub fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn wda_unsigned_cache(data_dir: &Path) -> PathBuf {
    data_dir.join("wda/WDA-unsigned.ipa")
}

fn env_num<T: std::str::FromStr>(k: &str, default: T) -> T {
    env(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub fn from_env(data_dir: &Path) -> Self {
        let screen_fallback =
            match (env("SCREEN_W").and_then(|v| v.parse().ok()), env("SCREEN_H").and_then(|v| v.parse().ok())) {
                (Some(width), Some(height)) => Some(ScreenSize { width, height }),
                _ => None,
            };
        Self {
            data_dir: data_dir.to_owned(),
            // 实际值由命令行给（--socket / ICTL_SOCKET，见 apps/iphone-control）
            socket: crate::protocol::default_socket(),
            udid: env("IPHONE_UDID"),
            manage_usbmuxd: env("ICTL_MANAGE_USBMUXD")
                .map_or_else(crate::protocol::running_as_root, |v| v != "0" && v != "false"),
            usbmuxd_bin: env("ICTL_USBMUXD_BIN").unwrap_or_else(|| "usbmuxd".into()),
            wda_unsigned: env("ICTL_WDA_UNSIGNED").map(PathBuf::from).unwrap_or_else(|| {
                let image = Path::new(crate::wda_pkg::IMAGE_PATH);
                if image.is_file() { image.to_owned() } else { wda_unsigned_cache(data_dir) }
            }),
            ddi_url: env("ICTL_DDI_URL").unwrap_or_else(|| ictl_device::ddi::DEFAULT_BASE_URL.into()),
            ddi_dir: env("ICTL_DDI_DIR").map(PathBuf::from),
            anisette_url: env("ICTL_ANISETTE_URL"),
            screen_fallback,
            resign_min: Duration::from_secs(env_num("RESIGN_MIN_DAYS", 2u64) * 86400),
            resign_check: Duration::from_secs(env_num("RESIGN_CHECK_HOURS", 6u64) * 3600),
            session_check: Duration::from_secs(24 * 3600),
            offline_alert: Duration::from_secs(env_num("OFFLINE_ALERT_MIN", 10u64) * 60),
            notify: NotifyConfig {
                tg_bot_token: env("TG_BOT_TOKEN"),
                tg_chat_id: env("TG_CHAT_ID"),
                bark_server: env("BARK_SERVER").unwrap_or_else(|| "https://api.day.app".into()),
                bark_key: env("BARK_KEY"),
            },
        }
    }

    pub fn logs(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn account_dir(&self) -> PathBuf {
        self.data_dir.join("account")
    }
    pub fn wda_dir(&self) -> PathBuf {
        self.data_dir.join("wda")
    }
    pub fn wda_signed(&self) -> PathBuf {
        self.wda_dir().join("WDA-signed.ipa")
    }
    /// 未签名的 WDA 用的是数据目录里下载来的那份（不是镜像自带或 ICTL_WDA_UNSIGNED 指定的）
    pub fn wda_unsigned_is_cache(&self) -> bool {
        self.wda_unsigned == wda_unsigned_cache(&self.data_dir)
    }
    /// 守护进程自己下载的开发者镜像缓存（没设 `ICTL_DDI_DIR` 时用）
    pub fn ddi_cache(&self) -> PathBuf {
        self.data_dir.join("ddi")
    }
    /// 旧版本的锁屏密码：只有一份，不知道是哪台设备的。现在不再读取、不再改名。
    /// 只有用户明确执行 `passcode clear` 时才删除。
    pub fn legacy_passcode_file(&self) -> PathBuf {
        self.data_dir.join("secrets/passcode")
    }
    pub fn passcodes_dir(&self) -> PathBuf {
        self.data_dir.join("secrets/passcodes")
    }
    /// 建好 [`Self::passcodes_dir`]，只有自己能进
    pub fn ensure_passcodes_dir(&self) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let dir = self.passcodes_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(dir)
    }
    /// 这台设备的锁屏密码。UDID 来自 usbmuxd，只有字母、数字和 `-`；别的字符换掉，不让它变成路径
    pub fn passcode_file_for(&self, udid: &str) -> PathBuf {
        let name: String = udid.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        self.passcodes_dir().join(name)
    }

    /// 数据目录里的明文锁屏密码文件（只列路径，不读内容）。
    pub fn plaintext_passcode_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if self.legacy_passcode_file().is_file() {
            out.push(self.legacy_passcode_file());
        }
        if let Ok(rd) = std::fs::read_dir(self.passcodes_dir()) {
            for entry in rd.flatten() {
                if entry.path().is_file() {
                    out.push(entry.path());
                }
            }
        }
        out
    }

    /// 删除用户点名的明文密码文件。`all` 连同旧的单文件一起删；否则只删这一台的文件。
    /// 返回删掉的路径，不返回内容。
    pub fn delete_plaintext_passcodes(&self, udid: Option<&str>, all: bool) -> std::io::Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        let targets: Vec<PathBuf> = if all {
            self.plaintext_passcode_paths()
        } else if let Some(udid) = udid {
            let path = self.passcode_file_for(udid);
            if path.is_file() { vec![path] } else { Vec::new() }
        } else {
            Vec::new()
        };
        for path in targets {
            std::fs::remove_file(&path)?;
            removed.push(path);
        }
        Ok(removed)
    }
    pub fn unlock_disabled_flag(&self) -> PathBuf {
        self.logs().join("UNLOCK-DISABLED")
    }
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// 建好数据目录并收紧权限：配对记录、账号私钥、锁屏密码都在里面
    pub fn prepare_dirs(&self) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for d in ["lockdown", "account", "wda", "ddi", "secrets", "logs"] {
            std::fs::create_dir_all(self.data_dir.join(d))?;
        }
        for d in ["", "lockdown", "account", "secrets"] {
            std::fs::set_permissions(self.data_dir.join(d), std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}
