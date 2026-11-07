# Auto-unlock

Off by default. When enabled, MCP actions unlock the phone with your saved passcode if it is locked, and so do the read-only screenshot, element-list and `iphone_wait_for` tools. Nothing is unlocked while WDA is unavailable.

::: danger Understand the risk first
The passcode is kept **only in the daemon's memory**. It is not written to the data directory, and a restart clears it. Anyone who can call this MCP server, or who is the same user or root while the daemon is running, can still unlock the phone. Only enable it on a host you fully control, preferably with a phone dedicated to automation.
:::

## Enable

```bash
docker exec -it iphone-control iphone-control passcode set
```

The daemon must be running. The passcode is kept for the device it is controlling; for another one add `--device <UDID or name>`. Numeric passcodes of 4 to 32 digits only. It is not saved to disk. To disable:

```bash
docker exec -it iphone-control iphone-control passcode clear           # the controlled device; --device for another
docker exec -it iphone-control iphone-control passcode clear --all     # all devices
```

## Several devices

Passcodes are **kept per device in memory**, so one device's passcode is never typed into another; a device without one needs a person to unlock it. The `devices` command (or the `iphone_list_devices` tool) shows which devices have one for this process.

Older versions wrote plaintext files (`secrets/passcode`, `secrets/passcodes/<UDID>`). Those files are no longer read or moved. They stay on disk until you delete them with `passcode clear` (one device) or `passcode clear --all`. The command prints paths, not the passcode. Run `passcode set` again, with the daemon up, to enable auto-unlock.

## Safeguards

- After every digit it checks how many digits the passcode field shows and stops at once if they don't match (an incomplete entry is not a wrong attempt); only a first digit that didn't register is retried, up to 3 rounds.
- If the phone is still locked after the full passcode, auto-unlock is **disabled immediately** and you are notified. It never tries a second time, so it can't pile up wrong attempts and trigger data erasure.
- From the second digit on, a stop marker is written first (`logs/UNLOCK-DISABLED` in the data directory) and removed only once the phone is confirmed unlocked or the entry is confirmed incomplete. So an error during entry, or the process being killed, also leaves auto-unlock disabled.
- The marker is **global**: a failure on one device disables auto-unlock on all of them.
- The first unlock after a reboot (BFU) always needs a person.
- A phone charging on its side goes into **StandBy**, which doesn't bring up the passcode keypad, so auto-unlock fails. Turn it off under *Settings → StandBy* when you use auto-unlock.

## Re-enable after it was disabled

Unlock the phone by hand first. If the passcode was wrong (for example, you changed it), save the new one and reset:

```bash
docker exec -it iphone-control iphone-control passcode set
docker exec -it iphone-control iphone-control unlock-reset
```

If it stopped because of an error during entry or a killed process, the passcode may be fine; once you've checked, `unlock-reset` alone is enough. `unlock-reset` doesn't check the passcode, and `passcode set` and `passcode clear` don't lift the stop. Don't delete `logs/` to get going again: that removes this safeguard.

::: tip Don't need auto-unlock?
Set *Settings → Display & Brightness → Auto-Lock* to *Never* on the phone, and it won't lock by itself.
:::
