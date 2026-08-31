# Service operation

The example service runs Netband in the foreground under systemd as a non-root dynamic
user. systemd owns `/var/lib/netband`, stdout is disabled, and operational stderr goes
to journald. Measurements remain exclusively in the configured CSV file.

## Install

Install a checksum-verified Linux x86_64 or aarch64 binary from the latest GitHub
release, then fetch the example service files from GitHub:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband-installer.sh | \
  sudo env NETBAND_INSTALL_DIR=/usr/local/bin sh
netband config check

work="$(mktemp -d)"
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband.toml \
  -o "$work/netband.toml"
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband.service \
  -o "$work/netband.service"
sudo install -Dm0644 "$work/netband.toml" /etc/netband/netband.toml
sudo install -Dm0644 "$work/netband.service" /etc/systemd/system/netband.service
rm -rf "$work"
sudo systemd-analyze verify /etc/systemd/system/netband.service
sudo systemctl daemon-reload
sudo systemctl enable --now netband.service
```

## Build from source

Building requires Git and Rust 1.98 or newer:

```sh
git clone https://github.com/gregl83/netband.git
cd netband
cargo build --release --locked
./target/release/netband config check
sudo install -Dm0755 target/release/netband /usr/local/bin/netband
sudo install -Dm0644 packaging/netband.toml /etc/netband/netband.toml
sudo install -Dm0644 packaging/netband.service /etc/systemd/system/netband.service
sudo systemd-analyze verify /etc/systemd/system/netband.service
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

The unit uses `DynamicUser=yes`, `StateDirectory=netband`, and only `CAP_NET_RAW` in its
ambient/bounding capability set. `ProtectSystem=strict` makes the state directory the
persistent writable location. Keep `TimeoutStopSec` longer than Netband's configured
`shutdown_grace` (30 seconds by default).

The unit deliberately specifies `--console off`, `StandardOutput=null`, and
`StandardError=journal`. TTY detection is not a service boundary: those settings ensure
measurement records are not duplicated into journald while startup, failures,
scheduler decisions, and shutdown diagnostics remain available there.

## ICMP permissions

Netband first uses the socket behavior provided by `surge-ping`. The reviewed unit
grants `CAP_NET_RAW`, which is sufficient for raw ICMP and selected-interface binding
without running the process as root. Do not grant broader networking capabilities.

For interactive non-root use, Linux ping datagram sockets can be enabled for a chosen
group range. Check the current range and your group ID:

```sh
sysctl net.ipv4.ping_group_range
id -g
```

An administrator may persist a range that includes the intended user's group, for
example a single group ID 1000:

```sh
printf '%s\n' 'net.ipv4.ping_group_range = 1000 1000' | \
  sudo tee /etc/sysctl.d/90-netband-ping.conf
sudo sysctl --system
```

Use the narrowest group range appropriate for the host. Interface binding or local
kernel policy may still require `CAP_NET_RAW`; use the systemd unit rather than making
the binary setuid or running the service as root.

## Exit codes

| Code | Meaning | systemd behavior |
| --- | --- | --- |
| 0 | Success, including completed first-signal shutdown | No restart after an intentional stop |
| 1 | One-shot measurement completed with probe/provider failure | Restart applies only if used as service command |
| 2 | Invalid configuration or command use | Restart; inspect configuration log |
| 3 | Permission denied | Restart; fix filesystem/socket permissions |
| 4 | Journal, scheduler state, or process-lock failure | Restart; check ownership, corruption, or second instance |
| 5 | Internal task or signal-handler failure | Restart |
| 6 | Forced shutdown after second signal or expired grace | Restart |

`Restart=on-failure` restarts only nonzero exits. `systemctl stop` sends SIGTERM;
Netband stops admitting work, cancels/drains active network work, flushes journal/state,
and returns zero within the configured grace period. A second signal or grace timeout
returns 6 after a final best-effort flush.

## Troubleshooting

**Configuration fails before startup**

Run the installed binary against the exact file and inspect stderr:

```sh
/usr/local/bin/netband --config /etc/netband/netband.toml config check
```

Relative paths resolve from the service working directory, so service configs should
use `/var/lib/netband/...` absolute paths. Unknown keys and inaccessible output parents
are rejected.

**Permission-denied ping rows or exit 3**

Confirm the unit still has `AmbientCapabilities=CAP_NET_RAW` and
`CapabilityBoundingSet=CAP_NET_RAW`, then inspect kernel/service restrictions:

```sh
systemctl cat netband.service
systemctl show netband.service -p AmbientCapabilities -p CapabilityBoundingSet
journalctl -u netband.service -p warning
```

For shell use, check `ping_group_range` as described above. Confirm every configured
interface exists, is up, and has an address suitable for each target family.

**Exit 4 or repeated restart**

Only one process may append an explicit CSV or own a scheduler state. Stop duplicate
units/manual runs. Check `/var/lib/netband` ownership through `systemctl status`; do not
delete lock files while a process is running. An incompatible CSV header requires a new
output file or an explicit migration, not manual header editing.

**Bandwidth never starts**

`config check` reports `bandwidth.automatic_enabled`. M-Lab requires explicit policy
acceptance; `daily_max=0` and `--no-bandwidth` disable starts. Inspect scheduler events
in CSV and operational logs for daily cap, minimum spacing, pending-trigger TTL,
provider cooldown, or five-attempt expiry. Direct endpoints never fall back to M-Lab.

**No measurements in journald**

This is expected. The unit sends stdout to null. Inspect `/var/lib/netband/netband.csv`
with a CSV reader; journald contains operational diagnostics only.

## State recovery

Netband fails closed when initialized scheduler state is missing or corrupt. It does not
recreate daily allowances. Recovery must preserve the reservation ledger.

1. Stop `netband.service` and confirm no Netband process holds `scheduler.lock`.
2. Preserve `scheduler.json`, `scheduler.bak`, `scheduler.initialized`, and
   `scheduler.reservations.jsonl` before making changes.
3. Validate `scheduler.json` as JSON. If it is missing or invalid, validate
   `scheduler.bak`, then copy the backup to `scheduler.json` without deleting the
   initialization marker or reservation ledger.
4. Restart the service. `StateDirectory` restores dynamic-user ownership.
5. Confirm the startup log reports a state flush and the retained `daily_runs_used`
   does not exceed the configured provider maximum.

Never recover by deleting the initialization marker or reservation ledger. If neither
state file is valid, retain all files and wait until the next UTC day or reconstruct the
allowance conservatively before restarting.

## Raspberry Pi

Use a 64-bit Raspberry Pi Linux image (`aarch64-unknown-linux-gnu`) and the pre-built
binary above, or follow [Build from source](#build-from-source). Before a release is
tagged, run [the release smoke](release.md) on real Pi hardware; CI's QEMU aarch64
execution validates architecture/startup compatibility but not the board's kernel,
interfaces, capabilities, thermals, or sustained NDT7 behavior.
