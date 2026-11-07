# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: **[Report a vulnerability](https://github.com/Derrors/iphone-control/security/advisories/new)** (Security tab → *Report a vulnerability*). Do not open a public issue for security problems. Reports in English or Chinese are both fine.

You can expect an acknowledgement within a week. Fixes go into the latest release; older versions are not patched while the project is at 0.x.

## What this software has access to

iphone-control is, by design, a remote control for a phone. Anyone who can talk to it can see the screen and do anything the phone's owner can do while it is unlocked. Treat the machine that runs it like the phone itself.

| Asset | Where | Who can reach it |
|---|---|---|
| Phone control (taps, typing, screenshots, apps) | Control socket: `/run/iphone-control/ctl.sock` inside the container, `$XDG_RUNTIME_DIR/iphone-control/ctl.sock` for a source install on Linux, `~/.local/share/iphone-control/ctl.sock` on macOS; mode 0600 | Root on the host; anyone who can `docker exec` into the container; the user running a source install or the macOS binary |
| WebDriverAgent (no authentication of its own) | On the phone, reached through usbmuxd; nothing listens on the network unless you run `wda-bridge` | `wda-bridge` binds 127.0.0.1 and requires a bearer token from a mode-0600 file. With the default Compose network, that listener is inside the container. Host networking (opt-in, for a proxy on the host's loopback) puts it on the host's loopback, where other users still need the token |
| Apple ID session tokens, development signing key, anisette state | `account/` in the data directory, AES-256-GCM sealed. The key is in the Linux kernel user keyring, not in the file. Mode 0600 is not encryption | Same user and root can read the keyring and process memory. A copy of the data directory without that keyring key does not reveal new tokens. The Apple ID password itself is never stored. Reboot or container recreate drops the key; log in again |
| Pairing records | `lockdown/` in the data directory (container), the system's `/var/lib/lockdown` (source install on Linux) or `/var/db/lockdown` (macOS, shared with Finder) | Root on the host. A pairing record lets its holder talk to the phone over USB without another *Trust* prompt |
| Lock-screen passcodes | Daemon memory only, after `passcode set` while the daemon is running. Not written to the data directory. Older plaintext files under `secrets/` are left in place, not read, and not deleted unless you run `passcode clear` | Same user and root, while the process is alive (memory, ptrace, or the control socket). Gone when the daemon or container stops |
| Notification credentials (Bark key, Telegram token) | `.env` or `~/.config/iphone-control/env` | Whoever can read that file |

The data directory is `./data` next to `compose.yaml` for the container (root-owned, mode 0700) and `~/.local/share/iphone-control` for a source install and on macOS.

## Design choices that limit exposure

- No network listener by default. WebDriverAgent is reached through usbmuxd. `wda-bridge` binds 127.0.0.1, forwards a request only when the Host header is a loopback name **and** `Authorization: Bearer` matches the token in the mode-0600 file next to the control socket, and is meant to be stopped after use. There is no switch to turn that check off.
- TLS certificate validation is on for anisette and Apple requests. Invalid certificates fail closed. There is no bypass flag.
- The control socket is a Unix socket with mode 0600, not TCP. The daemon also checks the peer uid: only its own user, or root, is accepted.
- The container is not `privileged` and sets `no-new-privileges`. It is only allowed USB character devices (major 189). Compose does **not** use host networking unless you opt in. The tunnel to the phone uses a user-space TCP stack.
- Passwords and verification codes are read only after the terminal echo is turned off. If that fails, the program stops and does not read the secret.
- New session tokens and signing keys are sealed before they touch the data directory. If the kernel keyring cannot hold the key, login refuses to save them instead of writing plaintext. `iphone-control keyring-check` and `doctor` try an ephemeral synthetic key (create, read, revoke) before any Apple login. A failed check stops login before the Apple ID password is sent, and before it is even prompted on the CLI.
- Auto-unlock is off by default. When enabled, it stops for good after a single wrong attempt (it writes a stop marker, `logs/UNLOCK-DISABLED`, before entering the second digit) and sends a notification; it never retries a passcode that might be wrong. The marker is global: a failure on one device stops auto-unlock on all of them until you run `unlock-reset`. It does not work before the first unlock after a reboot (BFU) — that always needs a person.
- Dependencies are pinned to exact versions, `Cargo.lock` is committed, base images are pinned by digest, and the WebDriverAgent download is checked against a sha256. CI runs gitleaks on every push.

## Known risks and recommendations

- **The container still runs as root.** It needs read-write access to the USB device nodes and must restart `usbmuxd` after the phone is re-plugged (the container has no udev). Dropping all capabilities or running as a non-root user was not validated on a phone and is not the default. `no-new-privileges` is on. Keep the host itself locked down.
- **The whole USB bus is mounted.** `/dev/bus/usb` is the bus, not one phone, so the container can see other USB devices the cgroup allows (major 189). Hot-plug is why it is not narrowed to a single node.
- **Third-party anisette server.** Apple ID logins need "anisette" data that normally only Apple hardware produces. By default it comes from a public server (`https://mac-ani.jkcoxson.com`). The server does not receive your Apple ID password, but your logins depend on an identity it provisions, and it sees when you log in. Set `ICTL_ANISETTE_URL` to an [anisette-v3 server](https://github.com/Dadoum/anisette-v3-server) you run yourself. That server must present a certificate the default trust store accepts. There is no option to skip certificate checks.
- **Use a dedicated Apple ID.** Whoever can use the unsealed session (same user or root, while the keyring key exists) can manage that account's development certificates and devices. A separate free account limits the damage.
- **Auto-unlock is memory-only, not safe against the same user.** The passcode is not written to disk anymore. Root and the same user can still read the daemon's memory or talk to the control socket while it runs. The passcode is also what protects the phone's data. Prefer leaving Auto-Lock off on a phone dedicated to automation. A daemon restart clears it.
- **Agents act with your hands.** An agent with these tools can send messages, make purchases and change settings. The MCP server tells agents to ask before paying, deleting, sending messages or changing account or privacy settings, but that is an instruction, not an enforcement mechanism.
- **Back up the data directory securely**, and never commit it. `.gitignore` and `.dockerignore` exclude `data/`, `.env` and key material. A backup of `account/` without the kernel keyring key does not contain newly sealed tokens in the clear. Leftover `*.legacy-plaintext` files and old `secrets/passcode*` files **do**. They are not deleted automatically. Remove them yourself: `iphone-control credentials discard-legacy --yes` and `iphone-control passcode clear --all`.

## Keyring preflight

Docker's published default seccomp profile blocks `add_key` and `keyctl` because the kernel keyring is not namespaced ([Docker seccomp docs](https://docs.docker.com/engine/security/seccomp/)). That is the documented default, not a measurement of a particular engine. `keyring-check` is the runtime measurement. It does not log in, open USB, or write a credential.

Run it on the host as your normal user, and once inside a one-shot container that does not start the daemon (the daemon would start a second usbmuxd). Do not add `seccomp=unconfined`, extra capabilities, or a plaintext fallback.

```bash
# Host, normal user. Does not start a daemon and does not open USB.
iphone-control keyring-check

# Already-built image, one-shot. No USB mount, no network, default seccomp.
# Does not start usbmuxd. Replace the image name if yours is not iphone-control:local.
docker run --rm --network none --entrypoint iphone-control iphone-control:local keyring-check
```

If the container reports `add_key` / `keyctl` as `Operation not permitted` and the host command succeeds, use the native unprivileged binary with the host's usbmuxd (`ICTL_MANAGE_USBMUXD` stays unset or `0`). Do not stop the host usbmuxd to make the container the USB owner just so login can run as root. A container that runs as root and is allowed to use the keyring would put the sealing key in host UID 0's keyring. The native user process keeps it in that user's keyring.

## Remaining trust

This is not a complete security boundary. Nothing here was checked against a physical iPhone.

- **Anisette providers are still trusted.** The default public server (or `ICTL_ANISETTE_URL`) provisions the identity used at Apple login and can see that a login is happening. A hostile or compromised provider can break login or supply a device identity you do not control. Certificate checks stop a network attacker from impersonating that server with a bad certificate; they do not make the provider itself trustworthy.
- **Signature renewal still depends on the saved developer session and on Apple.** Re-signing WebDriverAgent uses the sealed session and the development key. When the kernel key is gone you must log in again. Apple can revoke certificates, expire profiles (about 7 days on a free account), or reject a signature. A pairing record still lets its holder talk to a trusted phone over USB.
- **Same-user and root are still in scope.** Mode 0600, a private tmpfs directory, and the kernel keyring do not stop the same uid or root. They can ptrace the process, read `/proc`, call `keyctl`, read the bridge token file, or `docker exec` into the container. Sealing is there so a stolen disk or backup is not the same thing as a stolen session. It is not encryption against the account that runs the daemon.
- **Screen data goes to the MCP client.** Screenshots, the element tree, and some typed-text mismatch errors are returned to whatever client launched `iphone-control mcp` (for example Claude). That client, and the model provider behind it, can see what was on the phone. This program cannot enforce a privacy policy on that path.
- **There is no 100% guarantee.** Bugs, a malicious dependency, a trusted certificate authority, a compromised anisette host, or physical access to the machine can still expose the phone. Do not treat this fork as a formal audit or as proof the device is safe.

## Developer Disk Image and WebDriverAgent sources

- The Developer Disk Image is downloaded at runtime from a community-maintained mirror (the same source pymobiledevice3 uses) unless you set `ICTL_DDI_URL` or `ICTL_DDI_DIR`. The phone verifies Apple's signature through personalization (TSS), so a tampered image will not mount.
- The unsigned WebDriverAgent comes from Appium's official release (pinned version, sha256-checked) and is signed locally with your Apple ID.
