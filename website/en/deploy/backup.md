# Data and Backups

## What's in the data directory

| Path | Contents | If lost |
|---|---|---|
| `account/` | Apple ID session, signing key and certificate | Log in again; a new certificate is created |
| `lockdown/` | Pairing records (Docker) | Tap *Trust* on the phone again |
| `wda/` | Signed WebDriverAgent | Re-signed automatically |
| `ddi/` | Developer Disk Image cache | Downloaded again |
| `secrets/passcodes/` | Per-device passcodes (only with auto-unlock) | Run `passcode set` again |
| `state.json` | Signed WDA details (Team ID, bundle id, profile expiry), selected device, screen size of the most recent device, last signing result | The daemon starts as if WDA isn't signed: run `sign` or `setup` again, and `select` again if you had selected a device |
| `logs/` | Logs, auto-unlock's stop marker `UNLOCK-DISABLED` | The logs don't matter. Losing the marker removes a safeguard: after a wrong passcode, the same one could be entered again |

Location: `./data` in the repository directory for Docker (owned by root, mode 0700); `~/.local/share/iphone-control` for a source install, with pairing records in the system's `/var/lib/lockdown`. On macOS the data directory is also `~/.local/share/iphone-control`, and the system keeps pairing records in `/var/db/lockdown` (shared with Finder).

::: danger These are secrets
The data directory can still connect to the phone without another Trust prompt (pairing records). Newly stored Apple ID sessions are sealed and are not usable without the kernel keyring key from that boot. Older plaintext copies (`*.legacy-plaintext`, `secrets/passcode*`) are not deleted for you and do contain secrets. Never commit or share the directory, and keep backups encrypted. The passcode is no longer written here.
:::

## Back up

```bash
docker compose stop
sudo tar czf iphone-control-data.tar.gz data     # owned by root, hence sudo
docker compose start
```

Source install:

```bash
systemctl --user stop iphone-control
tar czf iphone-control-data.tar.gz -C ~/.local/share iphone-control
sudo tar czf lockdown.tar.gz /var/lib/lockdown     # pairing records
systemctl --user start iphone-control
```

macOS: stop the daemon with Ctrl-C in its terminal, then run the second `tar` above. Pairing records belong to the system and don't need a backup; on a new Mac, tap *Trust* on the phone once more.

## Move to a new host

Restore the data directory (and `/var/lib/lockdown` for a source install) to the same location on the new host and start it; no new pairing or login is needed.
