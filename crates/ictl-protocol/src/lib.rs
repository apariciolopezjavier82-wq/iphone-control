//! 守护进程控制接口：Unix socket（0600，默认位置见 [`default_socket`]），每行一个 JSON。
//! MCP 进程和 CLI 都通过它访问设备；本机不开 TCP 端口。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 以 root 运行（容器、系统服务）时的控制接口和数据目录
pub const SYSTEM_SOCKET: &str = "/run/iphone-control/ctl.sock";
pub const SYSTEM_DATA_DIR: &str = "/var/lib/iphone-control";

pub fn running_as_root() -> bool {
    // SAFETY: geteuid 没有前置条件，也不会失败
    unsafe { libc::geteuid() == 0 }
}

/// 默认数据目录：root 用 [`SYSTEM_DATA_DIR`]；普通用户（从源码运行）用 `$XDG_DATA_HOME/iphone-control`，
/// 即 `~/.local/share/iphone-control`
pub fn default_data_dir() -> PathBuf {
    if running_as_root() {
        return SYSTEM_DATA_DIR.into();
    }
    let xdg = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    match xdg
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| PathBuf::from(h).join(".local/share")))
    {
        Some(base) => base.join("iphone-control"),
        None => SYSTEM_DATA_DIR.into(),
    }
}

/// 默认控制接口：root 用 [`SYSTEM_SOCKET`]；普通用户用 `$XDG_RUNTIME_DIR/iphone-control/ctl.sock`，
/// 没有这个变量时放进默认数据目录。守护进程、MCP、CLI 用同一个函数算，不用另外配置就能找到彼此
pub fn default_socket() -> PathBuf {
    if running_as_root() {
        return SYSTEM_SOCKET.into();
    }
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(run) => PathBuf::from(run).join("iphone-control/ctl.sock"),
        None => default_data_dir().join("ctl.sock"),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// MCP 的 iphone_status：与 toolkit 相同的文字
    Status,
    /// 区域单位为点；None 表示整屏
    Screenshot {
        region: Option<(f64, f64, f64, f64)>,
    },
    ListElements {
        filter: String,
        limit: usize,
        /// 守护进程缓存的元素树不超过这么旧就直接用，不再读（0 = 总是重读）。
        /// 用于「等界面稳定」之后紧接着列元素：Settle 刚读过
        #[serde(default)]
        max_age_ms: u64,
    },
    Tap {
        x: f64,
        y: f64,
    },
    TapElement {
        text: String,
        index: usize,
        /// 同 ListElements。调用方声明这一串点击不改变界面布局（例如在同一个键盘上连按）时才给
        #[serde(default)]
        max_age_ms: u64,
    },
    LongPress {
        x: f64,
        y: f64,
        seconds: f64,
    },
    Swipe {
        from: (f64, f64),
        to: (f64, f64),
        seconds: f64,
    },
    TypeText {
        text: String,
        submit: bool,
    },
    PressButton {
        button: String,
    },
    ListApps {
        include_system: bool,
    },
    LaunchApp {
        app: String,
    },
    TerminateApp {
        app: String,
    },
    OpenUrl {
        url: String,
    },
    /// 等界面稳定：连续两次读到的元素一样，最多等 `max_ms`。操作后顺带返回界面时先调它
    Settle {
        max_ms: u64,
    },
    /// 查屏幕上有没有挡着的系统弹窗：有就返回提醒文字，没有返回空文字。不带 then 的操作做完后调
    AlertCheck,
    /// 等包含 `text` 的元素出现，最多等 `timeout_ms`（代替 agent 反复截图轮询）
    WaitFor {
        text: String,
        timeout_ms: u64,
    },
    /// 运维：守护进程和设备的状态快照（JSON 文本）
    Daemon,
    /// 运维：逐层诊断，返回多行文字
    Doctor,
    Resign {
        force: bool,
    },
    UnlockReset,
    /// 列出 USB 上接着的全部设备，标出正在控制哪台
    ListDevices,
    /// 改为控制另一台设备：`device` 是 UDID、设备名或 `auto`（取消选定，改回自动选）
    SelectDevice {
        device: String,
    },
    /// 把这台设备的锁屏密码放进守护进程内存。不写磁盘，重启后失效。
    SetPasscode {
        udid: String,
        passcode: String,
    },
    /// 清掉内存中的密码，并删除数据目录里调用方点名的明文密码文件。
    ClearPasscode {
        udid: Option<String>,
        #[serde(default)]
        all: bool,
    },
}

/// 控制接口只接受守护进程自己的用户，以及 root（容器里 `docker exec`、排障）。
/// 其他本机用户即使碰到了权限过宽的 socket 也进不来。
pub fn control_peer_allowed(peer_uid: u32, daemon_euid: u32) -> bool {
    peer_uid == daemon_euid || peer_uid == 0
}

pub fn current_euid() -> u32 {
    unsafe { libc::geteuid() }
}

/// 本进程之后创建的文件默认只有所有者能读写。已有文件的权限不变。
pub fn tighten_umask() {
    unsafe { libc::umask(0o077) };
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Text {
        text: String,
    },
    /// JPEG base64 + 说明文字（尺寸、放大倍数、坐标换算式）
    Image {
        jpeg_base64: String,
        caption: String,
    },
    Error {
        message: String,
    },
}

impl Response {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
    pub fn error(s: impl Into<String>) -> Self {
        Self::Error { message: s.into() }
    }
}

impl Request {
    /// 会改变手机状态的操作。守护进程忙时排队太久就放弃，免得点击落到已经变了的屏幕上
    pub fn is_device_action(&self) -> bool {
        matches!(
            self,
            Self::Tap { .. }
                | Self::TapElement { .. }
                | Self::LongPress { .. }
                | Self::Swipe { .. }
                | Self::TypeText { .. }
                | Self::PressButton { .. }
                | Self::LaunchApp { .. }
                | Self::TerminateApp { .. }
                | Self::OpenUrl { .. }
                | Self::SelectDevice { .. }
        )
    }
}

/// 工具结果里报告系统弹窗挡着屏幕的固定写法（后面接「弹窗文字」）。MCP 端据此判断结果里是否已经报过弹窗，
/// 不能只找「系统弹窗」几个字：按名字点到的元素文字里也可能有
pub const ALERT_BLOCKING: &str = "屏幕上有系统弹窗";

/// `iphone_tap_element` 没点到时结果开头的几种写法（不算出错，单独调用时是普通结果；批量里要停下）
pub const TAP_MISSED: [&str; 3] = ["没找到", "只匹配到", "被挡住"];

/// 请求里带上这个键（值为 true）：客户端发完请求不半关闭连接，守护进程据此把「读到 EOF」当成调用方已经走了，
/// 还在排队等设备的请求就不再执行。旧版本的客户端发完就半关闭、不带这个键，守护进程对它们照旧处理完。
/// 放在请求对象里而不是改协议格式：旧版本守护进程会忽略这个多出来的键
pub const CANCEL_ON_CLOSE: &str = "cancel_on_close";

/// 请求里带上这个键（值为 UDID）：守护进程正在控制的不是这台就不执行。`iphone_batch` 用它防止中途换了设备
/// （自动选设备时拔掉正在控制的那台，会改控另一台）后，剩下的步骤落到另一台上
pub const EXPECT_UDID: &str = "expect_udid";

/// 发一个请求、读一个响应（客户端用）。`timeout` 为 None 时一直等（例如 `sign` 要等签名装完）。
/// 返回之前被丢掉（超时、调用方取消）时连接随之关闭，守护进程会放弃还在排队的这个请求
pub async fn call(
    socket: &std::path::Path,
    req: &Request,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<Response> {
    call_expecting(socket, req, timeout, None).await
}

/// 同 [`call`]；`expect_udid` 见 [`EXPECT_UDID`]
pub async fn call_expecting(
    socket: &std::path::Path,
    req: &Request,
    timeout: Option<std::time::Duration>,
    expect_udid: Option<&str>,
) -> std::io::Result<Response> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let exchange = async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let (r, mut w) = stream.into_split();
        let mut v = serde_json::to_value(req)?;
        if let Some(o) = v.as_object_mut() {
            o.insert(CANCEL_ON_CLOSE.into(), true.into());
            if let Some(u) = expect_udid {
                o.insert(EXPECT_UDID.into(), u.into());
            }
        }
        let mut line = v.to_string();
        line.push('\n');
        w.write_all(line.as_bytes()).await?;
        let mut resp = String::new();
        BufReader::new(r).read_line(&mut resp).await?;
        serde_json::from_str(&resp).map_err(std::io::Error::other)
    };
    match timeout {
        None => exchange.await,
        Some(t) => tokio::time::timeout(t, exchange).await.map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("守护进程 {} 秒内没有应答（可能正忙，例如正在启动 WDA 或重签）", t.as_secs()),
            )
        })?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_as_tagged_json() {
        let r = Request::Swipe { from: (1.0, 2.0), to: (3.0, 4.0), seconds: 0.3 };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.starts_with(r#"{"op":"swipe""#), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        let e: Response = serde_json::from_str(r#"{"kind":"error","message":"x"}"#).unwrap();
        assert_eq!(e, Response::error("x"));
        let set = Request::SetPasscode { udid: "UDID".into(), passcode: "0000".into() };
        let s = serde_json::to_string(&set).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), set);
    }

    #[test]
    fn control_socket_rejects_other_users() {
        assert!(control_peer_allowed(1000, 1000));
        assert!(control_peer_allowed(0, 1000));
        assert!(!control_peer_allowed(1001, 1000));
    }

    /// 旧版本守护进程直接反序列化请求：多出来的 cancel_on_close 键不能让它报错（含没有字段的请求）
    #[test]
    fn extra_cancel_key_is_ignored_when_parsing() {
        for s in [r#"{"op":"status","cancel_on_close":true}"#, r#"{"op":"tap","x":1.0,"y":2.0,"cancel_on_close":true}"#]
        {
            serde_json::from_str::<Request>(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        }
    }
}
