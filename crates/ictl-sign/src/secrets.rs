//! 会话令牌和开发私钥的封存。
//!
//! 文件权限 0600 不是加密：同一用户、root、数据目录的备份都能读到明文。
//! 这里把 `accounts.json`、`state.plist` 和 `keys/` 下的文件用 AES-256-GCM 封存后再写入数据目录。
//! 密钥只放在 Linux 内核用户密钥环里，不写进数据目录，重启或容器重建后密钥就没了，需要重新登录。
//! 解封后的明文只放在 tmpfs（`/dev/shm` 或 `XDG_RUNTIME_DIR`）的 0700 目录里，用完覆盖并删除。
//!
//! 旧的明文文件不会被悄悄删掉：第一次成功封存时复制为 `*.legacy-plaintext`（0600），原路径改成密文。
//! 之后只读密文。解不开密文时不会退回读取那份明文副本。

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use zeroize::Zeroize;

use crate::SignError;

const MAGIC: &[u8] = b"ICTLSEAL1\n";
const KEY_DESC: &str = "ictl-account-seal-v1";

pub struct SealKey([u8; 32]);

impl Drop for SealKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SealKey {
    fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

enum KeyLoad {
    Ready(SealKey),
    Unavailable(String),
}

pub struct LegacyDiscard {
    pub found: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

/// 一次临时密钥环探测的结果。`summary` 只有结论和系统错误，没有密钥字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyringCheck {
    pub ok: bool,
    pub summary: String,
}

impl KeyringCheck {
    /// 登录在调用苹果之前必须先通过。失败时返回的文字就是 [`Self::summary`]。
    pub fn require_for_apple_login(&self) -> Result<(), String> {
        if self.ok { Ok(()) } else { Err(self.summary.clone()) }
    }
}

/// 创建、读回并撤销一把临时合成密钥。不写文件，不用账号数据，不碰 USB。
/// 成功也不保留这把密钥。失败时登录必须停在联系苹果之前，并且不能改成明文存储。
pub fn keyring_preflight() -> KeyringCheck {
    match ephemeral_key_roundtrip() {
        Ok(()) => KeyringCheck {
            ok: true,
            summary:
                "内核用户密钥环可用：已创建、读回并撤销一把临时合成密钥。密钥不保留，检查没有使用账号、USB 或真实秘密。\
登录在联系苹果之前会再确认一次；确认失败就不会把会话写成明文。"
                    .into(),
        },
        Err(summary) => KeyringCheck { ok: false, summary },
    }
}

pub(crate) struct RuntimeAccount {
    root: PathBuf,
    durable: PathBuf,
    key: Option<SealKey>,
    key_error: Option<String>,
    legacy_left: RefCell<Vec<PathBuf>>,
}

impl std::fmt::Debug for RuntimeAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAccount")
            .field("durable", &self.durable)
            .field("key", &self.key.as_ref().map(|_| "redacted"))
            .finish()
    }
}

impl RuntimeAccount {
    pub(crate) fn open(durable: &Path) -> Result<Self, SignError> {
        Self::open_with(durable, load_key())
    }

    fn open_with(durable: &Path, key: KeyLoad) -> Result<Self, SignError> {
        fs::create_dir_all(durable)?;
        set_mode(durable, 0o700)?;
        let root = runtime_dir()?;
        let (key, key_error) = match key {
            KeyLoad::Ready(k) => (Some(k), None),
            KeyLoad::Unavailable(e) => (None, Some(e)),
        };
        let view = Self { root, durable: durable.to_owned(), key, key_error, legacy_left: RefCell::new(Vec::new()) };
        let mut saw_plaintext = false;
        for src in list_secret_files(&view.durable)? {
            let mut bytes = fs::read(&src)?;
            let dest = view.root.join(rel_under(&view.durable, &src)?);
            if bytes.starts_with(MAGIC) {
                let Some(key) = view.key.as_ref() else {
                    bytes.zeroize();
                    return Err(SignError::Secrets(
                        "sealed credentials could not be opened because the Linux kernel keyring key is gone \
(reboot or container restart drops it). Refusing to read any .legacy-plaintext copy and refusing to write \
session tokens or signing keys as plaintext. Run iphone-control login again after the keyring is available. \
Existing files were not deleted."
                            .into(),
                    ));
                };
                let mut plain = open_sealed(key, &bytes).map_err(|e| {
                    bytes.zeroize();
                    SignError::Secrets(format!(
                        "{e}. Refusing to fall back to a legacy plaintext file. Nothing was deleted."
                    ))
                })?;
                bytes.zeroize();
                write_private(&dest, &plain)?;
                plain.zeroize();
            } else if !bytes.is_empty() {
                write_private(&dest, &bytes)?;
                bytes.zeroize();
                saw_plaintext = true;
            } else {
                bytes.zeroize();
            }
        }
        if saw_plaintext && view.key.is_some() {
            view.persist()?;
        }
        Ok(view)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn ensure_key(&self) -> Result<(), SignError> {
        if self.key.is_some() {
            Ok(())
        } else {
            Err(SignError::Secrets(
                self.key_error
                    .clone()
                    .unwrap_or_else(|| "refusing to write session tokens or signing keys as plaintext".into()),
            ))
        }
    }

    pub(crate) fn persist(&self) -> Result<(), SignError> {
        let Some(key) = self.key.as_ref() else {
            return Err(SignError::Secrets(
                self.key_error
                    .clone()
                    .unwrap_or_else(|| "refusing to write session tokens or signing keys as plaintext".into()),
            ));
        };
        for src in list_secret_files(&self.root)? {
            let mut plain = fs::read(&src)?;
            let rel = rel_under(&self.root, &src)?;
            let dest = self.durable.join(&rel);
            if dest.is_file() {
                let mut existing = fs::read(&dest)?;
                if !existing.starts_with(MAGIC) && !existing.is_empty() {
                    let legacy = legacy_sibling(&dest);
                    if !legacy.exists() {
                        write_private(&legacy, &existing)?;
                        self.legacy_left.borrow_mut().push(legacy);
                    }
                }
                existing.zeroize();
            }
            let sealed = seal(key, &plain);
            plain.zeroize();
            write_private(&dest, &sealed)?;
        }
        Ok(())
    }
}

impl Drop for RuntimeAccount {
    fn drop(&mut self) {
        if self.key.is_some()
            && let Err(e) = self.persist()
        {
            tracing::warn!("未能把凭据封存进数据目录，没有改回明文写入：{e}");
        }
        wipe_tree(&self.root);
    }
}

/// 数据目录里仍能直接读到的明文副本。只返回路径，不读出内容。
pub fn credential_warnings(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(files) = list_secret_files(dir) else {
        return out;
    };
    for path in files {
        match file_kind(&path) {
            FileKind::Missing | FileKind::Sealed | FileKind::Empty => {}
            FileKind::Plaintext => out.push(format!(
                "凭据文件仍是明文（0600 不是加密），程序没有删除它：{}。内核密钥环可用时会封存并留下 *.legacy-plaintext 副本。",
                path.display()
            )),
        }
    }
    if let Ok(legacy) = list_named(dir, ".legacy-plaintext") {
        for path in legacy {
            out.push(format!(
                "仍留着封存前的明文副本，程序不会读取也不会自动删除：{}。确认登录正常后运行 iphone-control credentials discard-legacy --yes",
                path.display()
            ));
        }
    }
    out
}

/// `delete == false` 只列出 `*.legacy-plaintext`。`delete == true` 才删除这些文件。
/// 不删除仍在使用的 `accounts.json` / `key.pem`，也不打印文件内容。
pub fn discard_legacy_plaintext(dir: &Path, delete: bool) -> Result<LegacyDiscard, SignError> {
    let found = list_named(dir, ".legacy-plaintext")?;
    let mut deleted = Vec::new();
    if delete {
        for path in &found {
            wipe_file(path);
            if path.exists() {
                return Err(SignError::Secrets(format!("无法删除 {}", path.display())));
            }
            deleted.push(path.clone());
        }
    }
    Ok(LegacyDiscard { found, deleted })
}

fn seal(key: &SealKey, plaintext: &[u8]) -> Vec<u8> {
    let unbound = UnboundKey::new(&AES_256_GCM, &key.0).expect("AES-256 key length");
    let aead = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    fill_random(&mut nonce_bytes).expect("urandom");
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut body = plaintext.to_vec();
    aead.seal_in_place_append_tag(nonce, Aad::empty(), &mut body).expect("seal");
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&body);
    nonce_bytes.zeroize();
    body.zeroize();
    out
}

fn open_sealed(key: &SealKey, blob: &[u8]) -> Result<Vec<u8>, String> {
    let rest = blob.strip_prefix(MAGIC).ok_or("not a sealed credential")?;
    if rest.len() < NONCE_LEN + 16 {
        return Err("sealed credential is truncated".into());
    }
    let (nonce_bytes, ct) = rest.split_at(NONCE_LEN);
    let unbound = UnboundKey::new(&AES_256_GCM, &key.0).map_err(|_| "invalid sealing key")?;
    let aead = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes).map_err(|_| "invalid nonce")?;
    let mut body = ct.to_vec();
    let plain = aead
        .open_in_place(nonce, Aad::empty(), &mut body)
        .map_err(|_| "sealed credential failed authentication; refusing to use it".to_string())?;
    let out = plain.to_vec();
    body.zeroize();
    Ok(out)
}

fn ephemeral_key_roundtrip() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        return Err(keyring_unavailable("keyring", &io::Error::new(io::ErrorKind::Unsupported, "not Linux")));
    }
    #[cfg(target_os = "linux")]
    {
        let desc =
            match std::ffi::CString::new(format!("ictl-preflight-{}-{}", std::process::id(), uuid::Uuid::new_v4())) {
                Ok(desc) => desc,
                Err(_) => return Err("临时密钥名无效".into()),
            };
        let mut payload = [0u8; 32];
        if let Err(e) = fill_random(&mut payload) {
            payload.zeroize();
            return Err(keyring_unavailable("urandom", &e));
        }
        // 加到会话密钥环：当前进程是持有者，才能读回。不放进用户密钥环，检查结束就作废。
        let id = match add_user_key(&desc, &payload, KEY_SPEC_SESSION_KEYRING) {
            Ok(id) => id,
            Err(e) => {
                payload.zeroize();
                return Err(keyring_unavailable("add_key", &e));
            }
        };
        let got = match read_user_key(id) {
            Ok(bytes) => bytes,
            Err(e) => {
                payload.zeroize();
                discard_key(id);
                return Err(keyring_unavailable("keyctl", &e));
            }
        };
        let matches = got.iter().zip(payload.iter()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
        payload.zeroize();
        let mut got = got;
        got.zeroize();
        discard_key(id);
        if !matches {
            return Err(keyring_unavailable("keyctl", &io::Error::new(io::ErrorKind::InvalidData, "payload mismatch")));
        }
        for ring in [KEY_SPEC_SESSION_KEYRING, KEY_SPEC_USER_KEYRING] {
            match search_keyring(ring, &desc) {
                Ok(None) => {}
                Ok(Some(stuck)) => {
                    discard_key(stuck);
                    return Err(
                        "临时合成密钥在撤销后仍能被找到。登录不会联系苹果，也不会把会话写成明文。没有保留这把密钥的内容。"
                            .into(),
                    );
                }
                Err(e) => return Err(keyring_unavailable("keyctl", &e)),
            }
        }
        Ok(())
    }
}

fn keyring_unavailable(op: &str, err: &io::Error) -> String {
    format!(
        "内核用户密钥环不可用（{op}: {err}）。登录不会联系苹果，也不会把会话或签名私钥写成明文。\
Docker 文档里的默认 seccomp 会拦截 add_key、keyctl 和 request_key，因为内核密钥环没有命名空间；\
EPERM 也可能是别的限制，这一次检查不能单独证明就是 seccomp。\
不要放宽 seccomp，也不要改成明文。请在宿主机上用普通用户运行 iphone-control keyring-check（沿用系统 usbmuxd，不要停掉它，也不要同时再起容器里的 usbmuxd）。"
    )
}

fn load_key() -> KeyLoad {
    match load_kernel_key() {
        Ok(k) => KeyLoad::Ready(k),
        Err(e) => KeyLoad::Unavailable(e),
    }
}

#[cfg(target_os = "linux")]
fn load_kernel_key() -> Result<SealKey, String> {
    let desc = std::ffi::CString::new(KEY_DESC).unwrap();
    match search_keyring(KEY_SPEC_USER_KEYRING, &desc) {
        Err(e) => return Err(keyring_unavailable("keyctl", &e)),
        Ok(Some(id)) => match read_user_key(id) {
            Ok(bytes) => return Ok(SealKey::new(bytes)),
            // 上一版只加到用户密钥环、没有给同用户读权限。那种密钥读不出来，也从未用来封存成功。
            Err(e) if e.raw_os_error() == Some(libc::EACCES) => {
                let _ = unlink_from(id, KEY_SPEC_USER_KEYRING);
            }
            Err(e) => return Err(keyring_unavailable("keyctl", &e)),
        },
        Ok(None) => {}
    }
    let mut payload = [0u8; 32];
    if let Err(e) = fill_random(&mut payload) {
        payload.zeroize();
        return Err(keyring_unavailable("urandom", &e));
    }
    let id = match add_user_key(&desc, &payload, KEY_SPEC_SESSION_KEYRING) {
        Ok(id) => id,
        Err(e) => {
            payload.zeroize();
            return Err(keyring_unavailable("add_key", &e));
        }
    };
    // 持有者全能，同 UID 可读可搜索。其他用户没有权限。
    const PERM_SAME_USER_READ: libc::c_ulong = 0x3f0b_0000;
    if let Err(e) = keyctl2(KEYCTL_SETPERM, id, PERM_SAME_USER_READ as libc::c_long) {
        payload.zeroize();
        discard_key(id);
        return Err(keyring_unavailable("keyctl", &e));
    }
    if let Err(e) = keyctl2(KEYCTL_LINK, id, KEY_SPEC_USER_KEYRING) {
        payload.zeroize();
        discard_key(id);
        return Err(keyring_unavailable("keyctl", &e));
    }
    let _ = unlink_from(id, KEY_SPEC_SESSION_KEYRING);
    let stored = match search_keyring(KEY_SPEC_USER_KEYRING, &desc).and_then(|found| {
        found
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "sealing key was not linked"))
            .and_then(read_user_key)
    }) {
        Ok(bytes) => bytes,
        Err(e) => {
            payload.zeroize();
            discard_key(id);
            return Err(keyring_unavailable("keyctl", &e));
        }
    };
    let matches = stored.iter().zip(payload.iter()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
    payload.zeroize();
    if !matches {
        let mut stored = stored;
        stored.zeroize();
        discard_key(id);
        return Err(keyring_unavailable("keyctl", &io::Error::new(io::ErrorKind::InvalidData, "payload mismatch")));
    }
    Ok(SealKey::new(stored))
}

#[cfg(target_os = "linux")]
const KEYCTL_SETPERM: libc::c_long = 5;
#[cfg(target_os = "linux")]
const KEYCTL_LINK: libc::c_long = 8;
#[cfg(target_os = "linux")]
const KEYCTL_UNLINK: libc::c_long = 9;
#[cfg(target_os = "linux")]
const KEYCTL_SEARCH: libc::c_long = 10;
#[cfg(target_os = "linux")]
const KEYCTL_READ: libc::c_long = 11;
#[cfg(target_os = "linux")]
const KEYCTL_INVALIDATE: libc::c_long = 21;
#[cfg(target_os = "linux")]
const KEY_SPEC_SESSION_KEYRING: libc::c_long = -3;
#[cfg(target_os = "linux")]
const KEY_SPEC_USER_KEYRING: libc::c_long = -4;

#[cfg(target_os = "linux")]
fn add_user_key(desc: &std::ffi::CStr, payload: &[u8; 32], ring: libc::c_long) -> io::Result<libc::c_long> {
    let typ = std::ffi::CString::new("user").unwrap();
    let id = unsafe {
        libc::syscall(
            libc::SYS_add_key,
            typ.as_ptr(),
            desc.as_ptr(),
            payload.as_ptr(),
            payload.len() as libc::size_t,
            ring,
        )
    };
    if id < 0 { Err(io::Error::last_os_error()) } else { Ok(id) }
}

#[cfg(target_os = "linux")]
fn keyctl2(op: libc::c_long, id: libc::c_long, arg: libc::c_long) -> io::Result<()> {
    let rc = unsafe { libc::syscall(libc::SYS_keyctl, op, id, arg) };
    if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(target_os = "linux")]
fn read_user_key(id: libc::c_long) -> io::Result<[u8; 32]> {
    let mut buf = [0u8; 32];
    let n = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_READ, id, buf.as_mut_ptr(), buf.len()) };
    if n == 32 {
        Ok(buf)
    } else {
        let err = io::Error::last_os_error();
        buf.zeroize();
        Err(if n < 0 { err } else { io::Error::new(io::ErrorKind::InvalidData, format!("read {n} bytes")) })
    }
}

#[cfg(target_os = "linux")]
fn search_keyring(ring: libc::c_long, desc: &std::ffi::CStr) -> io::Result<Option<libc::c_long>> {
    let typ = std::ffi::CString::new("user").unwrap();
    let id = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_SEARCH, ring, typ.as_ptr(), desc.as_ptr(), 0) };
    if id >= 0 {
        Ok(Some(id))
    } else {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOKEY) { Ok(None) } else { Err(err) }
    }
}

#[cfg(target_os = "linux")]
fn unlink_from(id: libc::c_long, ring: libc::c_long) -> io::Result<()> {
    keyctl2(KEYCTL_UNLINK, id, ring)
}

#[cfg(target_os = "linux")]
fn discard_key(id: libc::c_long) {
    let _ = unlink_from(id, KEY_SPEC_SESSION_KEYRING);
    let _ = unlink_from(id, KEY_SPEC_USER_KEYRING);
    let _ = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_INVALIDATE, id) };
}

#[cfg(not(target_os = "linux"))]
fn load_kernel_key() -> Result<SealKey, String> {
    Err(
        "credential sealing uses the Linux kernel keyring; refusing to write session tokens or signing keys as plaintext"
            .into(),
    )
}

fn fill_random(buf: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(buf)
}

enum FileKind {
    Missing,
    Empty,
    Sealed,
    Plaintext,
}

fn file_kind(path: &Path) -> FileKind {
    let Ok(mut f) = File::open(path) else {
        return FileKind::Missing;
    };
    let mut buf = [0u8; 16];
    let n = f.read(&mut buf).unwrap_or(0);
    let kind = if n == 0 {
        FileKind::Empty
    } else if n >= MAGIC.len() && buf.starts_with(MAGIC) {
        FileKind::Sealed
    } else {
        FileKind::Plaintext
    };
    buf.zeroize();
    kind
}

fn list_secret_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for name in ["accounts.json", "state.plist"] {
        let path = root.join(name);
        if path.is_file() {
            out.push(path);
        }
    }
    let keys = root.join("keys");
    if keys.is_dir() {
        walk_files(&keys, &mut out)?;
    }
    Ok(out)
}

fn list_named(root: &Path, suffix: &str) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_dir() {
        walk_suffix(root, suffix, &mut out)?;
    }
    Ok(out)
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            walk_files(&entry.path(), out)?;
        } else if ft.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".legacy-plaintext") && !name.starts_with('.') {
                out.push(entry.path());
            }
        }
    }
    Ok(())
}

fn walk_suffix(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            walk_suffix(&entry.path(), suffix, out)?;
        } else if ft.is_file() && entry.file_name().to_string_lossy().ends_with(suffix) {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn rel_under(root: &Path, path: &Path) -> Result<PathBuf, SignError> {
    path.strip_prefix(root)
        .map(|p| p.to_owned())
        .map_err(|_| SignError::Secrets(format!("凭据路径超出目录 {}", root.display())))
}

fn legacy_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("secret");
    path.with_file_name(format!("{name}.legacy-plaintext"))
}

fn runtime_dir() -> io::Result<PathBuf> {
    let base = if Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        PathBuf::from(run)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no tmpfs for secret material (/dev/shm or XDG_RUNTIME_DIR); refusing to put plaintext on a persistent disk",
        ));
    };
    reap_dead_runtime_dirs(&base);
    let dir = base.join(format!("ictl-account-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir)?;
    set_mode(&dir, 0o700)?;
    Ok(dir)
}

fn reap_dead_runtime_dirs(base: &Path) {
    let Ok(rd) = fs::read_dir(base) else { return };
    for entry in rd.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some(rest) = name.strip_prefix("ictl-account-") else { continue };
        let Ok(pid) = rest.split('-').next().unwrap_or("").parse::<u32>() else { continue };
        if pid == 0 || pid == std::process::id() || pid_alive(pid) {
            continue;
        }
        wipe_tree(&entry.path());
    }
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        let _ = set_mode(parent, 0o700);
    }
    let tmp_name = format!(".{}.tmp", path.file_name().and_then(|s| s.to_str()).unwrap_or("secret"));
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let _ = set_mode(&tmp, 0o600);
    fs::rename(&tmp, path)?;
    let _ = set_mode(path, 0o600);
    Ok(())
}

fn wipe_tree(root: &Path) {
    if root.is_dir() {
        if let Ok(rd) = fs::read_dir(root) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    wipe_tree(&path);
                } else {
                    wipe_file(&path);
                }
            }
        }
        let _ = fs::remove_dir(root);
    } else if root.exists() {
        wipe_file(root);
    }
}

fn wipe_file(path: &Path) {
    if let Ok(meta) = fs::metadata(path)
        && meta.is_file()
        && let Ok(mut f) = OpenOptions::new().write(true).open(path)
    {
        let len = meta.len() as usize;
        let zeros = vec![0u8; len.min(1024 * 1024)];
        let mut left = len;
        while left > 0 {
            let n = left.min(zeros.len());
            if f.write_all(&zeros[..n]).is_err() {
                break;
            }
            left -= n;
        }
        let _ = f.sync_all();
    }
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SealKey {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = i as u8;
        }
        SealKey::new(b)
    }

    const TOKEN: &str = "SYNTHETIC-SESSION-TOKEN-7f3a9c";

    #[test]
    fn seal_roundtrip_hides_plaintext_and_rejects_tampering() {
        let key = test_key();
        let sealed = seal(&key, TOKEN.as_bytes());
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
        assert_eq!(open_sealed(&key, &sealed).unwrap(), TOKEN.as_bytes());
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let err = open_sealed(&key, &tampered).unwrap_err();
        assert!(!err.contains(TOKEN), "{err}");
        assert!(err.contains("authentication"), "{err}");
    }

    #[test]
    fn plaintext_is_sealed_and_legacy_copy_is_kept() {
        let dir = std::env::temp_dir().join(format!("ictl-seal-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("keys/TEAM")).unwrap();
        fs::write(dir.join("accounts.json"), format!(r#"{{"token":"{TOKEN}"}}"#)).unwrap();
        fs::write(dir.join("keys/TEAM/key.pem"), format!("-----BEGIN PRIVATE KEY-----\n{TOKEN}\n")).unwrap();
        {
            let view = RuntimeAccount::open_with(&dir, KeyLoad::Ready(test_key())).unwrap();
            let runtime = fs::read_to_string(view.root().join("accounts.json")).unwrap();
            assert!(runtime.contains(TOKEN));
            assert!(!view.legacy_left.borrow().is_empty());
        }
        let sealed = fs::read(dir.join("accounts.json")).unwrap();
        let key_sealed = fs::read(dir.join("keys/TEAM/key.pem")).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(key_sealed.starts_with(MAGIC));
        assert!(!sealed.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
        assert!(!key_sealed.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
        let legacy = fs::read_to_string(dir.join("accounts.json.legacy-plaintext")).unwrap();
        assert!(legacy.contains(TOKEN), "legacy copy must stay until the user deletes it");
        let warnings = credential_warnings(&dir).join("\n");
        assert!(warnings.contains("legacy-plaintext"), "{warnings}");
        assert!(!warnings.contains(TOKEN), "{warnings}");
        let listed = discard_legacy_plaintext(&dir, false).unwrap();
        assert!(!listed.found.is_empty());
        assert!(listed.deleted.is_empty());
        assert!(dir.join("accounts.json.legacy-plaintext").is_file());
        let removed = discard_legacy_plaintext(&dir, true).unwrap();
        assert!(!removed.deleted.is_empty());
        assert!(!dir.join("accounts.json.legacy-plaintext").exists());
        assert!(dir.join("accounts.json").is_file(), "sealed file must stay");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_key_does_not_read_legacy_or_rewrite_plaintext() {
        let dir = std::env::temp_dir().join(format!("ictl-seal-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let body = format!(r#"{{"token":"{TOKEN}"}}"#);
        fs::write(dir.join("accounts.json"), &body).unwrap();
        let unavailable = KeyLoad::Unavailable(
            "Linux kernel keyring is unavailable; refusing to write session tokens or signing keys as plaintext".into(),
        );
        {
            let view = RuntimeAccount::open_with(&dir, unavailable).unwrap();
            assert!(view.key.is_none());
            let err = view.ensure_key().unwrap_err().to_string();
            assert!(err.contains("refusing to write"), "{err}");
            assert!(!err.contains(TOKEN), "{err}");
        }
        assert_eq!(fs::read_to_string(dir.join("accounts.json")).unwrap(), body);
        assert!(!dir.join("accounts.json.legacy-plaintext").exists());

        let key = test_key();
        let sealed = seal(&key, TOKEN.as_bytes());
        drop(key);
        fs::write(dir.join("accounts.json"), &sealed).unwrap();
        fs::write(dir.join("accounts.json.legacy-plaintext"), &body).unwrap();
        let err = RuntimeAccount::open_with(
            &dir,
            KeyLoad::Unavailable("Linux kernel keyring is unavailable; refusing to write".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not read") || err.contains("could not be opened") || err.contains("gone"), "{err}");
        assert!(!err.contains(TOKEN), "{err}");
        assert_eq!(fs::read(dir.join("accounts.json")).unwrap(), sealed);
        assert_eq!(fs::read_to_string(dir.join("accounts.json.legacy-plaintext")).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preflight_uses_an_ephemeral_key_and_blocks_apple_login_on_failure() {
        let check = keyring_preflight();
        assert!(!check.summary.contains("unconfined"), "{}", check.summary);
        assert!(!check.summary.contains("SYNTHETIC-SESSION-TOKEN"), "{}", check.summary);
        if check.ok {
            assert!(check.require_for_apple_login().is_ok());
            assert!(check.summary.contains("撤销"), "{}", check.summary);
        } else {
            let mut contacted_apple = false;
            let err = (|| {
                check.require_for_apple_login()?;
                contacted_apple = true;
                Ok::<(), String>(())
            })()
            .unwrap_err();
            assert!(!contacted_apple);
            assert!(err.contains("不会联系苹果"), "{err}");
            assert!(err.contains("明文"), "{err}");
        }
    }

    #[test]
    fn blocked_keyring_does_not_offer_plaintext_or_a_weaker_seccomp() {
        let check =
            KeyringCheck { ok: false, summary: keyring_unavailable("add_key", &std::io::Error::from_raw_os_error(1)) };
        let mut contacted_apple = false;
        let err = (|| {
            check.require_for_apple_login()?;
            contacted_apple = true;
            Ok::<(), String>(())
        })()
        .unwrap_err();
        assert!(!contacted_apple);
        assert!(err.contains("add_key"));
        assert!(err.contains("不会联系苹果"));
        assert!(err.contains("明文"));
        assert!(!err.contains("unconfined"));
        assert!(!err.contains("danger_accept"));
        let src = include_str!("lib.rs");
        let login = src.split("pub async fn login").nth(1).unwrap();
        let login = login.split("pub async fn selected_email").next().unwrap();
        let gate = login.find("require_for_apple_login").expect("login must check the keyring");
        let apple = login.find("PlumeAccount::login").expect("login calls Apple");
        assert!(gate < apple, "keyring gate must run before Apple authentication");
    }

    #[test]
    fn linux_keyring_roundtrip_or_explicit_refusal() {
        match load_kernel_key() {
            Ok(key) => {
                let sealed = seal(&key, TOKEN.as_bytes());
                assert!(!sealed.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
                assert_eq!(open_sealed(&key, &sealed).unwrap(), TOKEN.as_bytes());
            }
            Err(e) => {
                assert!(e.contains("refusing to write"), "{e}");
                assert!(!e.contains(TOKEN), "{e}");
            }
        }
    }
}
