# Security and Privacy

iphone-control is a remote control for a phone: whoever can reach it can do anything the owner can do while the phone is unlocked. Treat the host like the phone itself.

## What is stored and who can reach it

| What | Where | Who can reach it |
|---|---|---|
| Phone control | Control socket (Unix socket, mode 0600) | Root on the host, anyone who can `docker exec` into the container, the same user for a source install |
| Apple ID session, signing key | `account/`, sealed with a key in the Linux kernel keyring. Mode 0600 is not encryption | Same user and root while the process and keyring key exist. The Apple ID password is never stored. Reboot or container recreate requires login again |
| Pairing records | `lockdown/` in the data directory (Docker) | Whoever can read it; it lets the holder talk to the phone over USB without another *Trust* prompt |
| Passcode (only with auto-unlock) | Daemon memory only. Old plaintext files are not read and not deleted unless you run `passcode clear` | Same user and root while the daemon is running |
| Notification keys | `.env` or `~/.config/iphone-control/env` | Whoever can read that file |

## Built-in protections

- **No network ports by default.** WDA is reached through usbmuxd. `wda-bridge` binds 127.0.0.1 and requires a bearer token from a mode-0600 file. Stop it when done.
- **TLS certificates are validated.** Invalid certificates fail closed. There is no bypass.
- **Not privileged**: the container only gets USB character devices.
- **Auto-unlock is off by default** and disables itself after one wrong attempt.
- Exact dependency versions, base images pinned by digest, and the WebDriverAgent download is sha256-checked.

## Risks to know about

- **The container runs as root** and mounts the USB bus. It needs the USB device nodes and must restart usbmuxd after re-plugging (there is no udev in the container). Compose does not use host networking unless you opt in.
- **The anisette server.** Apple ID logins need anisette data, which by default comes from a public third-party server (`mac-ani.jkcoxson.com`). It never gets your password, but your logins depend on the identity it provisions and it can see when you log in. To avoid it, run your own [anisette-v3-server](https://github.com/Dadoum/anisette-v3-server) and set `ICTL_ANISETTE_URL`. That server's certificate must validate. A trusted certificate does not make the provider trustworthy.
- **Use a dedicated Apple ID.** The session and signing key, once unsealed, are enough to manage that account's development certificates and devices.
- **Same user and root still win.** Sealing and a memory-only passcode do not stop the user who runs the daemon, or root. Screenshots and the UI tree are sent to the MCP client (and therefore to the model provider). This is not a 100% guarantee, and it was not validated on a physical iPhone. Signature renewal still depends on Apple and on that saved session. Details: [SECURITY.md](https://github.com/Derrors/iphone-control/blob/main/SECURITY.md).
- **The agent acts with your hands.** It can send messages, buy things and change settings. The server tells agents to ask first for these, but that is an instruction, not enforcement.

## Disclaimer

This project is not affiliated with, endorsed by, or sponsored by Apple Inc. iPhone, iOS and Xcode are trademarks of Apple Inc. Only use it with devices and accounts you own or are authorized to control; automating third-party apps may violate their terms; Apple may limit free developer accounts. The software is provided "as is", without warranty of any kind.

Found a security problem? [Report it privately](https://github.com/Derrors/iphone-control/security/advisories/new). Full details: [SECURITY.md](https://github.com/Derrors/iphone-control/blob/main/SECURITY.md).
