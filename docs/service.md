# Netband service operation

The example service runs Netband in the foreground as a systemd dynamic user. systemd
owns `/var/lib/netband`, stdout is disabled, and operational stderr goes to the journal.
Measurements remain exclusively in the configured CSV file.

## Install

```sh
cargo build --release
sudo install -Dm0755 target/release/netband /usr/local/bin/netband
sudo install -Dm0644 packaging/netband.toml /etc/netband/netband.toml
sudo install -Dm0644 packaging/netband.service /etc/systemd/system/netband.service
sudo systemctl daemon-reload
sudo systemctl enable --now netband.service
```

Review `/etc/netband/netband.toml` before enabling bandwidth tests. M-Lab remains
disabled until its acceptable-use and privacy policies are explicitly accepted.

```sh
systemctl status netband.service
journalctl -u netband.service
sudo systemctl stop netband.service
sudo systemctl restart netband.service
```

The unit grants only `CAP_NET_RAW`, uses `DynamicUser=yes`, and makes the systemd state
directory the only persistent writable location. Keep `TimeoutStopSec` longer than
Netband's `shutdown_grace` value.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, including a completed first-signal shutdown |
| 1 | A one-shot measurement completed with a probe/provider failure |
| 2 | Invalid configuration or command use |
| 3 | Permission denied |
| 4 | Journal, scheduler state, or process-lock failure |
| 5 | Internal task or signal-handler failure |
| 6 | Forced shutdown after a second signal or expired grace period |

`Restart=on-failure` restarts only nonzero exits. An intentional `systemctl stop`
therefore remains stopped.

## State recovery

Netband fails closed when an initialized scheduler state file is missing or corrupt. It
does not recreate daily allowances. Recovery must preserve the reservation ledger.

1. Stop `netband.service` and confirm no Netband process holds `scheduler.lock`.
2. Preserve `scheduler.json`, `scheduler.bak`, `scheduler.initialized`, and
   `scheduler.reservations.jsonl` before making changes.
3. Validate `scheduler.json` as JSON. If it is missing or invalid, validate
   `scheduler.bak`, then copy the backup to `scheduler.json` without deleting the
   initialization marker or reservation ledger.
4. Restore ownership and mode through `systemctl restart netband.service`; systemd's
   `StateDirectory` manages the dynamic-user ownership.
5. Confirm the startup log reports a state flush and that the retained run count does
   not exceed the provider's daily maximum.

Never recover by deleting the initialization marker or reservation ledger. If neither
state file is valid, retain all files and wait until the next UTC day or reconstruct the
allowance conservatively before restarting.
