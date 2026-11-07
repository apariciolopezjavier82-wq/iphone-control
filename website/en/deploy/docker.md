# Docker

The recommended way to run it. The image holds one Rust binary plus Debian's usbmuxd, and the container is not `privileged`.

## Install

Pick any one of the three; they all end up with the same container.

### Just a compose file

No need to clone the repository — the least effort. Create a directory (e.g. `~/iphone-control`) and put a `compose.yaml` in it:

```yaml
services:
  iphone:
    image: ghcr.io/derrors/iphone-control:latest   # or pin a version, e.g. :0.1.4
    container_name: iphone-control
    # USB character devices only; no privileged needed
    device_cgroup_rules:
      - "c 189:* rmw"
    init: true
    stop_grace_period: 30s
    security_opt:
      - no-new-privileges:true
    # Private network by default. Uncomment only if the proxy is on the host's loopback:
    # network_mode: host
    restart: unless-stopped
    environment:
      # All optional — uncomment what you need; see "Configuration" for the full list
      # HTTP_PROXY: http://127.0.0.1:7890
      # HTTPS_PROXY: http://127.0.0.1:7890
      NO_PROXY: localhost,127.0.0.1,::1
      # BARK_SERVER: https://api.day.app
      # BARK_KEY: your-key
      # TG_BOT_TOKEN: "123456:ABC..."
      # TG_CHAT_ID: "your chat id"
    volumes:
      - /dev/bus/usb:/dev/bus/usb
      - ./data:/var/lib/iphone-control
```

Then, in that directory:

```bash
docker compose up -d
```

`./data` is relative to the directory holding `compose.yaml`; it contains the pairing records and the Apple ID session, so **back it up**. Instead of `environment:` you can put the variables in a `.env` next to it and add `env_file: .env`; if it holds notification keys, keep it out of git. See [Configuration](../guide/configuration) for what each variable does.

### Build it yourself

```bash
git clone https://github.com/Derrors/iphone-control && cd iphone-control
cp .env.example .env
docker compose up -d --build
```

### Use the prebuilt image

Images are published at `ghcr.io/derrors/iphone-control` (amd64, arm64). In `.env`:

```ini
IMAGE=ghcr.io/derrors/iphone-control:latest   # or pin a version, e.g. :0.1.4
```

Then:

```bash
docker compose pull && docker compose up -d --no-build
```

Afterwards, run the [setup wizard](../guide/getting-started) once.

## What the compose file does

- Allows only USB character devices (`device_cgroup_rules: "c 189:* rmw"`) and bind-mounts `/dev/bus/usb`, so device nodes created after re-plugging show up in the container.
- Private Compose network by default, with `no-new-privileges`. Uncomment `network_mode: host` only when a proxy listens on the host's loopback. The container is still root and still bind-mounts the whole USB bus.
- Mounts `./data` at `/var/lib/iphone-control`, so pairing and login survive re-creating the container.
- `init: true` and `stop_grace_period: 30s` — on stop it closes the sessions on the phone first, so no "paired computer" icon is left behind.

## Upgrade

Read the upgrade notes in the [changelog](https://github.com/Derrors/iphone-control/blob/main/CHANGELOG.md) first.

```bash
docker compose pull && docker compose up -d             # just a compose file
# or
git pull
docker compose up -d --build                            # self-built
# or
docker compose pull && docker compose up -d --no-build  # prebuilt image
```

If the image tag pins a version, change it in `compose.yaml` to the new one before pulling.

Data lives in `./data` and is untouched. Restarting the container drops the MCP connection in your AI client; reconnect it (`/mcp` in Claude Code).

::: tip Wait until it's ready
WDA is ready about 10 seconds after a restart. Several restarts in a row can make the phone ask for the passcode for "XCTest" again — just enter it.
:::

## Everyday commands

```bash
docker exec iphone-control iphone-control status    # state
docker exec iphone-control iphone-control doctor    # diagnosis
docker logs iphone-control                          # logs
docker compose down                                 # stop (data is kept)
```
