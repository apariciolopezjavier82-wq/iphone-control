//! iphone-control：唯一的可执行程序。守护进程、MCP 服务和运维命令都在这里。
//!
//! 运维命令（status、doctor、resign、unlock-reset）经控制接口问守护进程；
//! setup、login、passcode 需要交互，直接在本进程里做。

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ictl_daemon::config::env as env_nonempty;
use ictl_daemon::protocol::{self, Request, Response};

#[cfg(feature = "debug-cmds")]
mod debug;

#[derive(Parser)]
#[command(name = "iphone-control", version, about = "在 Linux 或 macOS 上控制 iPhone，并提供 MCP 工具")]
struct Cli {
    /// 数据根目录（账号、描述文件、日志等）。默认：root 是 /var/lib/iphone-control，普通用户是 ~/.local/share/iphone-control
    #[arg(long, env = "ICTL_DATA_DIR", default_value_os_t = protocol::default_data_dir())]
    data_dir: PathBuf,
    /// 守护进程控制接口。默认：root 是 /run/iphone-control/ctl.sock，普通用户是 $XDG_RUNTIME_DIR/iphone-control/ctl.sock
    #[arg(long, env = "ICTL_SOCKET", default_value_os_t = protocol::default_socket())]
    socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 常驻守护进程（容器入口；从源码运行时由 systemd 或手动启动）
    Daemon,
    /// MCP stdio 服务（由 agent 拉起）
    Mcp,
    /// 打印把 MCP 注册到 Claude Code 的命令：在容器里打印 docker exec 的写法，否则直接调用本程序
    McpConfig {
        /// 容器名；给了就按容器部署打印
        #[arg(long)]
        container: Option<String>,
    },
    /// 守护进程和设备状态、最近的日志
    Status,
    /// 逐层诊断，每项失败都给出修复办法
    Doctor,
    /// 探测内核用户密钥环：创建、读回并撤销一把临时合成密钥。不登录、不碰 USB、不写账号数据
    KeyringCheck,
    /// 首次安装向导：配对 → 开发者模式 → 登录 Apple ID → 签名安装 WDA（可重复运行）
    Setup,
    /// 登录 Apple ID（密码和双重认证验证码只在这里输入）
    Login {
        /// Apple ID 邮箱；不给就交互输入
        email: Option<String>,
    },
    /// 立即签名并安装 WDA
    Sign,
    /// 检查描述文件，快过期就重签；--force 立即重签
    Resign {
        #[arg(long)]
        force: bool,
    },
    /// 设置/删除锁屏密码（自动解锁，默认关闭）
    Passcode {
        #[command(subcommand)]
        action: PasscodeAction,
    },
    /// 解除自动解锁的停用（输错或中途出错后停用）。只删停用标记，不核对密码：先确认保存的密码对，必要时 passcode set 重设
    UnlockReset,
    /// 列出 USB 上接着的设备，标出正在控制哪台
    Devices,
    /// 改为控制另一台设备，重启后还记得；auto 改回自动选（正在控制的那台断开才换）
    Select {
        /// UDID、设备名或 auto
        device: String,
    },
    /// 显式把 WDA 桥接到 127.0.0.1 供外部工具使用（默认不监听任何端口）
    WdaBridge {
        #[arg(long, default_value_t = 8100)]
        port: u16,
    },
    /// 真机诊断（开发用，要用 `--features debug-cmds` 编译）。`wda`/`e2e`/`hid` 会和守护进程抢 XCTest、HID，先停掉守护进程
    #[cfg(feature = "debug-cmds")]
    #[command(hide = true)]
    Debug(debug::DebugArgs),
    /// 处理封存前留下的明文凭据副本。默认只列出路径；`--yes` 才删除
    Credentials {
        #[command(subcommand)]
        action: CredentialsAction,
    },
    /// 按已配置的渠道发测试通知：每类事件各一条（标题带「[测试]」），或只发指定的一类
    NotifyTest {
        /// resign / session / pin / pin-recovered / unlock / offline / online；不给就全发
        event: Option<String>,
    },
}

/// 封存会话之后留下的明文副本
#[derive(Subcommand)]
enum CredentialsAction {
    /// 列出（或在 `--yes` 时删除）`account/` 里的 `*.legacy-plaintext`。不打印内容，也不删除仍在使用的文件
    DiscardLegacy {
        #[arg(long)]
        yes: bool,
    },
}

/// 锁屏密码按设备分开：一台的密码不会输进另一台。密码只进守护进程内存。
#[derive(Subcommand)]
enum PasscodeAction {
    Set {
        /// 哪台设备（UDID 或设备名）；不给就是守护进程正在控制的那台
        #[arg(long)]
        device: Option<String>,
    },
    Clear {
        /// 哪台设备（UDID 或设备名）；不给就是守护进程正在控制的那台
        #[arg(long)]
        device: Option<String>,
        /// 删掉所有设备的密码
        #[arg(long, conflicts_with = "device")]
        all: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 依赖树里 rustls 的 ring 和 aws-lc-rs 都开着，不装默认 provider 的话第一个 HTTPS 请求就 panic
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let quiet = matches!(cli.command, Command::Mcp);
    // ICTL_LOG 留空（.env 里常见的 `ICTL_LOG=`）当作没设；原来空值会生成一个什么都不放行的过滤器
    let filter =
        env_nonempty("ICTL_LOG").and_then(|v| tracing_subscriber::EnvFilter::try_new(v).ok()).unwrap_or_else(|| {
            tracing_subscriber::EnvFilter::new(if quiet { "warn" } else { "info,idevice=warn,apple_codesign=warn" })
        });
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(filter)
        .init();

    match cli.command {
        Command::Daemon => {
            let mut cfg = ictl_daemon::Config::from_env(&cli.data_dir);
            cfg.socket = cli.socket;
            ictl_daemon::run(cfg).await
        }
        Command::Mcp => ictl_mcp::serve_stdio(cli.socket).await,
        Command::McpConfig { container } => {
            println!("{}", mcp_config(container, &cli.socket)?);
            Ok(())
        }
        Command::Status => status(&cli.socket).await,
        Command::Doctor => print_text(&cli.socket, Request::Doctor).await,
        Command::KeyringCheck => keyring_check(),
        Command::Setup => setup(&cli).await,
        Command::Login { email } => login(&cli.data_dir, email).await,
        Command::Sign => print_text(&cli.socket, Request::Resign { force: true }).await,
        Command::Resign { force } => print_text(&cli.socket, Request::Resign { force }).await,
        Command::Passcode { action } => passcode(&cli.data_dir, &cli.socket, action).await,
        Command::Credentials { action } => credentials(&cli.data_dir, action),
        Command::UnlockReset => print_text(&cli.socket, Request::UnlockReset).await,
        Command::Devices => print_text(&cli.socket, Request::ListDevices).await,
        Command::Select { device } => print_text(&cli.socket, Request::SelectDevice { device }).await,
        Command::WdaBridge { port } => wda_bridge(&cli.socket, port).await,
        #[cfg(feature = "debug-cmds")]
        Command::Debug(args) => debug::run(env_nonempty("IPHONE_UDID").as_deref(), args).await,
        Command::NotifyTest { event } => notify_test(&cli.data_dir, event.as_deref()).await,
    }
}

async fn notify_test(data_dir: &Path, only: Option<&str>) -> anyhow::Result<()> {
    use ictl_daemon::notify::{Event, send};
    let cfg = ictl_daemon::Config::from_env(data_dir).notify;
    let channels =
        usize::from(cfg.bark_key.is_some()) + usize::from(cfg.tg_bot_token.is_some() && cfg.tg_chat_id.is_some());
    if channels == 0 {
        bail!(
            "没有配置通知渠道：填 BARK_KEY，或 TG_BOT_TOKEN + TG_CHAT_ID（容器部署在 .env 里填、重建容器；\
             从源码运行在守护进程的环境文件里填、重启守护进程）"
        );
    }
    let events: Vec<Event> =
        Event::samples().into_iter().filter(|e| only.is_none_or(|n| e.sample_name() == n)).collect();
    if events.is_empty() {
        let names: Vec<&str> = Event::samples().iter().map(|e| e.sample_name()).collect();
        bail!("没有这类事件，可选：{}", names.join(" / "));
    }
    let mut failed = 0;
    for e in &events {
        let (title, body) = e.title_body();
        let ok = send(&cfg, &format!("[测试] {title}"), &body).await;
        println!(
            "{} {:<14} {title}（{ok}/{channels} 个渠道成功）",
            if ok == channels { "✅" } else { "❌" },
            e.sample_name()
        );
        failed += usize::from(ok < channels);
        // Bark 同组消息太密会被手机合并，隔一下
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if failed > 0 {
        bail!("{failed} 条没有全部发出去，详见上面的 WARN 日志");
    }
    Ok(())
}

/// 容器部署：MCP 进程由 `docker exec -i` 在容器里拉起。从源码运行：直接调用本程序，
/// 显式带上 socket，免得 Claude Code 的环境里没有 XDG_RUNTIME_DIR 时算出别的位置
fn mcp_config(container: Option<String>, socket: &Path) -> anyhow::Result<String> {
    let in_container = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();
    let container = container
        .or_else(|| in_container.then(|| env_nonempty("CONTAINER_NAME").unwrap_or_else(|| "iphone-control".into())));
    Ok(match container {
        Some(c) => format!("claude mcp add iphone -s user -- docker exec -i {c} iphone-control mcp"),
        None => {
            let exe = std::env::current_exe().context("找不到本程序的路径")?;
            format!(
                "claude mcp add iphone -s user -- {} --socket {} mcp",
                shell_quote(&exe.to_string_lossy()),
                shell_quote(&socket.to_string_lossy())
            )
        }
    })
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@%".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

async fn ask(socket: &Path, req: Request) -> anyhow::Result<Response> {
    // 签名要等装完，不设时限；其余命令守护进程忙也最多等 3 分钟
    let timeout = match req {
        Request::Resign { .. } => None,
        _ => Some(Duration::from_secs(180)),
    };
    protocol::call(socket, &req, timeout).await.with_context(|| {
        format!(
            "连不上守护进程（{}）：先启动 iphone-control daemon（容器里是入口进程）；\
             守护进程以别的用户运行时，用 --socket 或 ICTL_SOCKET 指到它的控制接口",
            socket.display()
        )
    })
}

async fn print_text(socket: &Path, req: Request) -> anyhow::Result<()> {
    match ask(socket, req).await? {
        Response::Text { text } => {
            println!("{text}");
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        Response::Image { caption, .. } => {
            println!("{caption}");
            Ok(())
        }
    }
}

async fn snapshot(socket: &Path) -> anyhow::Result<ictl_daemon::Snapshot> {
    match ask(socket, Request::Daemon).await? {
        Response::Text { text } => Ok(serde_json::from_str(&text)?),
        Response::Error { message } => bail!("{message}"),
        Response::Image { .. } => bail!("守护进程返回了意外的响应"),
    }
}

async fn status(socket: &Path) -> anyhow::Result<()> {
    let s = snapshot(socket).await?;
    println!("状态      {}", s.state.describe());
    if let Some(d) = &s.device {
        println!("手机      {} {} iOS {}", d.udid, d.product_type, d.product_version);
    }
    let current = s.device.as_ref().map(|d| d.udid.as_str());
    let others: Vec<String> =
        s.devices.iter().filter(|d| Some(d.udid.as_str()) != current).map(ictl_daemon::core::device_label).collect();
    if !others.is_empty() {
        println!("另外接着  {}（iphone-control select 切换）", others.join("、"));
    }
    match (&s.pinned, s.pinned_by_env) {
        (Some(p), true) => println!("选设备    IPHONE_UDID 固定为 {p}"),
        (Some(p), false) => println!("选设备    选定了 {p}（iphone-control select auto 改回自动选）"),
        (None, _) if !others.is_empty() => println!("选设备    自动（正在控制的那台断开才换）"),
        (None, _) => {}
    }
    let p = &s.persisted;
    println!("WDA       {}", p.wda_bundle.as_deref().unwrap_or("未签名"));
    match &s.device_profile {
        ictl_daemon::core::DeviceProfile::Missing => {
            println!("描述文件  这台手机上没有装当前 Apple ID 签的 WDA（iphone-control setup）")
        }
        dp => {
            if let Some(left) = dp.left(p) {
                let whose = if matches!(dp, ictl_daemon::core::DeviceProfile::Expires(_)) {
                    "手机上的"
                } else {
                    "上次签的"
                };
                println!("描述文件  {whose}还剩 {} 小时", left.as_secs() / 3600);
            }
        }
    }
    if let Some(r) = &p.last_resign {
        println!("上次重签  {r}");
    }
    let unlock = match (s.auto_unlock, s.unlock_disabled) {
        (_, true) => "已停用（之前输错过，iphone-control unlock-reset）",
        (true, false) => "已启用",
        (false, false) => "未启用",
    };
    println!("自动解锁  {unlock}");
    println!("通知      {}", if s.notify_configured { "已配置" } else { "未配置" });
    println!("\n最近日志：");
    for l in s.log_tail {
        println!("  {l}");
    }
    Ok(())
}

fn read_line(prompt: &str) -> anyhow::Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("读不到输入（标准输入已关闭）：在终端里运行；容器部署用 docker exec -it");
    }
    Ok(s.trim().to_owned())
}

const SECRET_ECHO_REFUSAL: &str =
    "拒绝读取密码：无法关闭终端回显（没有控制终端，或 stty -echo 失败）。密码不会在可见输入下读取。";

/// 关不掉回显就失败。不读取输入。
fn secret_input_allowed(echo_hidden: bool) -> anyhow::Result<()> {
    if echo_hidden { Ok(()) } else { bail!(SECRET_ECHO_REFUSAL) }
}

/// 不回显地读一行。关不掉回显就拒绝，不读入密码。
fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok();
    let echo_hidden = match &tty {
        Some(tty) => std::process::Command::new("stty")
            .arg("-echo")
            .stdin(std::process::Stdio::from(tty.try_clone()?))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        None => false,
    };
    struct RestoreEcho;
    impl Drop for RestoreEcho {
        fn drop(&mut self) {
            if let Ok(tty) = std::fs::File::open("/dev/tty") {
                let _ = std::process::Command::new("stty")
                    .arg("echo")
                    .stdin(std::process::Stdio::from(tty))
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
    let _restore = echo_hidden.then_some(RestoreEcho);
    secret_input_allowed(echo_hidden)?;
    let secret = read_line(prompt)?;
    println!();
    Ok(secret)
}

/// 账号目录和 anisette 地址：与守护进程同一份配置（路径、环境变量的读法都在 Config 里）
fn account_dir(data_dir: &Path) -> ictl_sign::AccountDir {
    let cfg = ictl_daemon::Config::from_env(data_dir);
    ictl_sign::AccountDir { dir: cfg.account_dir(), anisette_url: cfg.anisette_url }
}

fn keyring_check() -> anyhow::Result<()> {
    let check = ictl_sign::keyring_preflight();
    println!("{}", check.summary);
    if check.ok { Ok(()) } else { bail!("{}", check.summary) }
}

async fn login(data_dir: &Path, email: Option<String>) -> anyhow::Result<()> {
    let check = ictl_sign::keyring_preflight();
    if !check.ok {
        bail!("{}", check.summary);
    }
    println!("建议用一个专门的免费 Apple ID，不要用主账号。");
    println!(
        "密码和验证码由本程序直接发给苹果；登录时会请求 anisette 服务生成设备标识（默认第三方，可用 ICTL_ANISETTE_URL 改成自建）。"
    );
    let email = match email {
        Some(e) => e,
        None => read_line("Apple ID 邮箱: ")?,
    };
    if email.is_empty() {
        bail!("没有输入 Apple ID 邮箱");
    }
    let password = read_secret("密码(输入不显示): ")?;
    if password.is_empty() {
        bail!("没有输入密码");
    }
    let two_factor = |req: ictl_sign::TwoFactorRequest| -> Result<ictl_sign::TwoFactorAction, String> {
        let sms = !req.trusted_phone_numbers.is_empty();
        let prompt = match req.method {
            ictl_sign::TwoFactorMethod::Sms => "短信验证码: ",
            ictl_sign::TwoFactorMethod::Device if sms => "设备上收到的验证码(输入 sms 改为短信接收): ",
            ictl_sign::TwoFactorMethod::Device => "设备上收到的验证码: ",
        };
        let code = read_secret(prompt).map_err(|e| e.to_string())?;
        if sms && code.eq_ignore_ascii_case("sms") {
            return Ok(ictl_sign::TwoFactorAction::SendSms(req.trusted_phone_numbers[0].id));
        }
        Ok(ictl_sign::TwoFactorAction::SubmitCode(code))
    };
    account_dir(data_dir).login(&email, &password, two_factor).await?;
    println!(
        "登录成功。会话令牌和开发私钥已用 Linux 内核密钥环封存到 {}，没有写成明文。",
        account_dir(data_dir).dir.display()
    );
    println!(
        "重启或重建容器后密钥环里的密钥会消失，需要重新登录。0600 不是加密；同一用户和 root 仍能读密钥环和进程内存。"
    );
    for note in ictl_sign::credential_warnings(&account_dir(data_dir).dir) {
        println!("注意：{note}");
    }
    Ok(())
}

/// 命令要针对哪台设备：`IPHONE_UDID` > 守护进程正在控制的（或选定的）> USB 上唯一的一台。
/// 给了 `query`（UDID 或设备名）就在接着的设备里找
async fn target_device(socket: &Path, query: Option<&str>) -> anyhow::Result<ictl_device::DeviceInfo> {
    let snap = snapshot(socket).await.ok();
    let mut infos = snap.as_ref().map(|s| s.devices.clone()).unwrap_or_default();
    if infos.is_empty() {
        for d in ictl_device::list_devices().await.unwrap_or_default() {
            let i = ictl_device::Device::from_usb(&d).info().await.unwrap_or_default();
            infos.push(ictl_device::DeviceInfo { udid: d.udid, ..i });
        }
    }
    let pick = |u: &str| {
        infos
            .iter()
            .find(|d| d.udid.eq_ignore_ascii_case(u))
            .cloned()
            .unwrap_or_else(|| ictl_device::DeviceInfo { udid: u.to_owned(), ..Default::default() })
    };
    if let Some(q) = query {
        let i = ictl_daemon::devices::resolve_device(&infos, q).map_err(anyhow::Error::msg)?;
        return Ok(infos[i].clone());
    }
    if let Some(u) = env_nonempty("IPHONE_UDID") {
        return Ok(pick(&u));
    }
    if let Some(s) = &snap {
        if let Some(d) = &s.device {
            return Ok(d.clone());
        }
        if let Some(p) = &s.pinned {
            return Ok(pick(p));
        }
    }
    match &infos[..] {
        [one] => Ok(one.clone()),
        [] => bail!("没接设备：用数据线接好并解锁"),
        _ => bail!("接着 {} 台设备，不知道是哪台：先用 iphone-control select 选一台，或加 --device 指定", infos.len()),
    }
}

fn credentials(data_dir: &Path, action: CredentialsAction) -> anyhow::Result<()> {
    let dir = account_dir(data_dir).dir;
    match action {
        CredentialsAction::DiscardLegacy { yes } => {
            let report = ictl_sign::discard_legacy_plaintext(&dir, yes)?;
            if report.found.is_empty() {
                println!("没有 *.legacy-plaintext 副本。");
            } else if yes {
                println!("已删除这些明文副本（内容未显示）：");
                for p in &report.deleted {
                    println!("  {}", p.display());
                }
            } else {
                println!("这些明文副本还在。内容未显示。确认后加 --yes 才会删除：");
                for p in &report.found {
                    println!("  {}", p.display());
                }
            }
            for note in ictl_sign::credential_warnings(&dir) {
                if !note.contains("legacy-plaintext") {
                    println!("注意：{note}");
                }
            }
        }
    }
    Ok(())
}

async fn passcode(data_dir: &Path, socket: &Path, action: PasscodeAction) -> anyhow::Result<()> {
    let cfg = ictl_daemon::Config::from_env(data_dir);
    match action {
        PasscodeAction::Clear { all, device } => {
            let udid = if all { None } else { Some(target_device(socket, device.as_deref()).await?.udid) };
            match protocol::call(
                socket,
                &Request::ClearPasscode { udid: udid.clone(), all },
                Some(Duration::from_secs(10)),
            )
            .await
            {
                Ok(Response::Text { text }) => println!("{text}"),
                Ok(Response::Error { message }) => bail!("{message}"),
                Ok(Response::Image { .. }) => bail!("守护进程返回了意外结果"),
                Err(e) => {
                    let removed = cfg.delete_plaintext_passcodes(udid.as_deref(), all)?;
                    println!("守护进程没在运行，内存中的密码不会保留。{e}");
                    if removed.is_empty() {
                        println!("数据目录里没有明文锁屏密码文件。");
                    } else {
                        println!("已删除这些明文文件（内容未显示）：");
                        for p in &removed {
                            println!("  {}", p.display());
                        }
                    }
                }
            }
        }
        PasscodeAction::Set { device } => {
            let d = target_device(socket, device.as_deref()).await?;
            println!("设备：{}", ictl_daemon::core::device_label(&d));
            println!("自动解锁：锁屏后，MCP 操作前会用这个密码解锁手机。密码只留在守护进程内存里，重启后要重新设置。");
            println!("风险：能调用这个 MCP 的人、同一用户和 root，都能在进程还活着时解锁手机。");
            println!("不会把密码写入数据目录。旧的明文文件留在原地，直到你运行 passcode clear。");
            println!("保护：每按一位都核对；整串输完仍锁着就停用自动解锁并推送通知，不会累计输错。只支持纯数字。");
            let p = read_secret("锁屏密码(输入不显示): ")?;
            if !(4..=32).contains(&p.len()) || !p.bytes().all(|b| b.is_ascii_digit()) {
                bail!("只支持 4 到 32 位的纯数字密码");
            }
            match protocol::call(
                socket,
                &Request::SetPasscode { udid: d.udid, passcode: p },
                Some(Duration::from_secs(10)),
            )
            .await
            {
                Ok(Response::Text { text }) => println!("{text}"),
                Ok(Response::Error { message }) => bail!("{message}"),
                Ok(Response::Image { .. }) => bail!("守护进程返回了意外结果"),
                Err(e) => bail!("守护进程没在运行，拒绝把锁屏密码写进文件：{e}"),
            }
            println!("别忘了在手机上把「自动锁定」从「永不」改回较短时间。");
        }
    }
    Ok(())
}

/// 每隔 `every` 检查一次，直到 `check` 返回 true 或超时
async fn wait_until<F, Fut>(timeout: Duration, every: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(every).await;
    }
}

async fn setup(cli: &Cli) -> anyhow::Result<()> {
    use ictl_device::{Device, DeviceError};
    let socket = &cli.socket;
    snapshot(socket).await.context("setup 需要守护进程在运行")?;

    println!("[1/6] 等待手机连接");
    let found = wait_until(Duration::from_secs(600), Duration::from_secs(5), || async {
        ictl_device::list_devices().await.map(|d| !d.is_empty()).unwrap_or(false)
    })
    .await;
    if !found {
        bail!("10 分钟内没检测到手机：用数据线接好并解锁手机");
    }
    // 和守护进程控制同一台：第 5 步的签名安装是守护进程对它正在控制的那台做的
    let target = target_device(socket, None).await?;
    let udid = Some(target.udid.clone());
    let device = Device::find(udid.as_deref()).await?;
    println!("  手机 {}", ictl_daemon::core::device_label(&target));

    println!("[2/6] 配对");
    if device.paired().await? {
        println!("  已配对");
    } else {
        println!("  手机上会弹出「要信任此电脑吗?」:点「信任」,再输入锁屏密码。");
        let ok = wait_until(Duration::from_secs(300), Duration::from_secs(5), || async {
            match device.pair("iphone-control").await {
                Ok(()) => true,
                Err(DeviceError::PairingPending | DeviceError::PasswordProtected) => false,
                Err(e) => {
                    eprintln!("  {e}");
                    false
                }
            }
        })
        .await;
        if !ok {
            bail!("5 分钟内没完成配对");
        }
        println!("  配对成功");
    }

    println!("[3/6] 开发者模式");
    if device.developer_mode_enabled().await.unwrap_or(false) {
        println!("  已打开");
    } else {
        let _ = device.reveal_developer_mode().await;
        println!("  请在手机上:设置 → 隐私与安全性 → 开发者模式 → 打开 → 重新启动;");
        println!("  重启后解锁,在弹窗里点「打开」并输入密码。手机重启期间连接会断开,会自动恢复。");
        let ok = wait_until(Duration::from_secs(900), Duration::from_secs(10), || async {
            match Device::find(udid.as_deref()).await {
                Ok(d) => d.developer_mode_enabled().await.unwrap_or(false),
                Err(_) => false,
            }
        })
        .await;
        if !ok {
            bail!("15 分钟内没检测到开发者模式打开");
        }
        println!("  已打开");
    }

    println!("[4/6] 登录 Apple ID");
    match account_dir(&cli.data_dir).selected_email().await {
        Ok(e) => println!("  已登录 {e}"),
        Err(_) => login(&cli.data_dir, None).await?,
    }

    println!("[5/6] 注册设备、签名 WDA 并安装到手机（手机要保持解锁）");
    // 前几步直接问 usbmuxd，重签却由守护进程做：它每 10 秒巡检一次才认到手机。手机刚重启完、usbmuxd 刚重启时
    // 这里会先看到手机，马上请求重签就报「手机未连接」，所以先等守护进程认到同一台
    let target = device.udid.clone();
    let seen = wait_until(Duration::from_secs(60), Duration::from_secs(2), || async {
        snapshot(socket).await.is_ok_and(|s| {
            s.state != ictl_daemon::machine::DeviceState::Absent && s.device.as_ref().is_none_or(|d| d.udid == target)
        })
    })
    .await;
    if !seen {
        bail!("守护进程 1 分钟内没认到手机 {}，运行 iphone-control doctor 看看卡在哪", device.udid);
    }
    print_text(socket, Request::Resign { force: true }).await?;

    println!("[6/6] 等待 WDA 启动");
    println!("  首次安装:在手机 设置 → 通用 → VPN 与设备管理 里找到你的 Apple ID,点「信任」(需要联网)。");
    println!("  如果弹出「为 XCTest 输入 iPhone 密码」,输入锁屏密码。");
    let ready = wait_until(Duration::from_secs(600), Duration::from_secs(10), || async {
        snapshot(socket).await.map(|s| s.state == ictl_daemon::machine::DeviceState::Ready).unwrap_or(false)
    })
    .await;
    println!("  {}", if ready { "WDA 就绪" } else { "10 分钟内 WDA 没就绪,运行 iphone-control doctor 看看卡在哪" });

    println!("\n可选设置");
    println!("  通知:填 TG_* 或 BARK_*(容器部署在 .env 里,然后重建容器;从源码运行在守护进程的环境文件里,然后重启它)");
    println!("  自动解锁:iphone-control passcode set");
    println!("  注册到 Claude Code:iphone-control mcp-config");
    println!();
    print_text(socket, Request::Doctor).await
}

/// 显式开启：127.0.0.1:port → 手机上的 WDA 8100。只绑本机回环地址。
/// WDA 本身没有认证。每个请求都要带本进程生成的令牌，并且 Host 必须是回环名。
/// 没有关闭认证的开关。用完就停。
async fn wda_bridge(socket: &Path, port: u16) -> anyhow::Result<()> {
    let target = target_device(socket, None).await?;
    let device = ictl_device::Device::find(Some(&target.udid)).await?;
    let token_path = bridge_token_path(socket);
    let token = write_bridge_token(&token_path)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("WDA 桥接 127.0.0.1:{port} → 手机 {} 的 8100 端口（Ctrl-C 停止）", device.udid);
    println!("令牌文件（0600，内容不在这里打印）：{}", token_path.display());
    println!("请求要带 Host 为 127.0.0.1 / localhost / [::1]，以及 Authorization: Bearer <令牌文件内容>。");
    println!("没有令牌的本机用户连不上。用完就停掉这个进程。");
    loop {
        let (client, _) = listener.accept().await?;
        let device = device.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge_conn(client, &device, &token).await {
                tracing::warn!("桥接连接出错：{e}");
            }
        });
    }
}

fn bridge_token_path(socket: &Path) -> PathBuf {
    socket.parent().unwrap_or_else(|| Path::new(".")).join("wda-bridge.token")
}

fn write_bridge_token(path: &Path) -> anyhow::Result<String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(token.as_bytes())?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

/// 先读到第一个请求的头部，Host 必须是本机回环地址才转发：恶意网页借 DNS rebinding 发到 127.0.0.1 的请求，
/// Host 头是它自己的域名
async fn bridge_conn(
    mut client: tokio::net::TcpStream,
    device: &ictl_device::Device,
    token: &str,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > 64 * 1024 {
            bail!("请求头太长");
        }
        let n = client.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    if !bridge_authorized(&head, token) {
        client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    }
    let mut remote = device.connect_port(8100).await?;
    remote.write_all(&head).await?;
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
    Ok(())
}

/// HTTP 请求头里的 Host 是不是 localhost / 127.0.0.1 / [::1]（端口不限）
fn host_is_loopback(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let Some(host) = text.split("\r\n").skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("host").then(|| v.trim().to_ascii_lowercase())
    }) else {
        return false;
    };
    let name = match host.strip_prefix('[') {
        Some(rest) => format!("[{}]", rest.split(']').next().unwrap_or_default()),
        None => host.split(':').next().unwrap_or_default().to_owned(),
    };
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn header_value(head: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    text.split("\r\n").skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

fn bridge_authorized(head: &[u8], token: &str) -> bool {
    if token.is_empty() || !host_is_loopback(head) {
        return false;
    }
    let Some(value) = header_value(head, "authorization") else {
        return false;
    };
    let Some(got) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) else {
        return false;
    };
    tokens_equal(got.trim(), token)
}

fn tokens_equal(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{bridge_authorized, host_is_loopback, secret_input_allowed, write_bridge_token};

    const TOKEN: &str = "synthetic-bridge-token";

    fn req(extra: &str) -> Vec<u8> {
        format!("GET /status HTTP/1.1\r\n{extra}\r\n\r\n").into_bytes()
    }

    #[test]
    fn bridge_requires_loopback_host_and_bearer_token() {
        assert!(host_is_loopback(b"GET /status HTTP/1.1\r\nHost: 127.0.0.1:8100\r\n\r\n"));
        assert!(!host_is_loopback(b"GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n"));
        let ok = req("Host: 127.0.0.1:8100\r\nAuthorization: Bearer synthetic-bridge-token");
        assert!(bridge_authorized(&ok, TOKEN));
        assert!(bridge_authorized(&req("Host: localhost\r\nAuthorization: bearer synthetic-bridge-token"), TOKEN));
        assert!(!bridge_authorized(&req("Host: 127.0.0.1\r\n"), TOKEN));
        assert!(!bridge_authorized(&req("Host: evil.example\r\nAuthorization: Bearer synthetic-bridge-token"), TOKEN));
        assert!(!bridge_authorized(&req("Host: 127.0.0.1\r\nAuthorization: Bearer wrong-token"), TOKEN));
        assert!(!bridge_authorized(&ok, ""));
        assert!(!bridge_authorized(
            &req("Host: localhost.evil.example\r\nAuthorization: Bearer synthetic-bridge-token"),
            TOKEN
        ));
    }

    #[test]
    fn login_checks_the_keyring_before_asking_for_the_password() {
        let src = include_str!("main.rs");
        let login = src.split("async fn login(").nth(1).expect("login fn");
        let login = login.split("async fn ").next().unwrap();
        let gate = login.find("keyring_preflight").expect("preflight");
        let prompt = login.find("read_secret").expect("password prompt");
        assert!(gate < prompt, "keyring check must run before the Apple ID password prompt");
    }

    #[test]
    fn secret_prompt_fails_closed_when_echo_cannot_be_hidden() {
        let err = secret_input_allowed(false).unwrap_err().to_string();
        assert!(err.contains("拒绝读取密码"), "{err}");
        assert!(secret_input_allowed(true).is_ok());
    }

    #[test]
    fn bridge_token_file_is_owner_only_and_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ictl-bridge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("wda-bridge.token");
        let token = write_bridge_token(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
        assert!(!token.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn compose_defaults_are_not_host_network_or_privileged() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let compose = std::fs::read_to_string(root.join("compose.yaml")).unwrap();
        for line in compose.lines() {
            let t = line.trim();
            if t.starts_with('#') {
                continue;
            }
            assert!(!t.starts_with("network_mode:"), "{line}");
            assert!(!t.starts_with("privileged:"), "{line}");
        }
        assert!(compose.contains("no-new-privileges:true"), "{compose}");
    }
}
