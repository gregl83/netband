# Service operation

The example service runs Netband in the foreground under systemd as a non-root dynamic
user. systemd owns `/var/lib/netband`, stdout is disabled, and operational stderr goes
to journald. Measurements remain exclusively in the configured CSV segments.

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

The unit uses `DynamicUser=yes`, `StateDirectory=netband netband/measurements`, and only
`CAP_NET_RAW` in its ambient/bounding capability set. `ProtectSystem=strict` makes the state directory the
persistent writable location. Keep `TimeoutStopSec` longer than Netband's configured
`shutdown_grace` (30 seconds by default).

The unit deliberately specifies `--console off`, `StandardOutput=null`, and
`StandardError=journal`. TTY detection is not a service boundary: those settings ensure
measurement records are not duplicated into journald while startup, failures,
scheduler decisions, and shutdown diagnostics remain available there.

## Measurements and logs

The packaged configuration writes rotating CSVs under
`/var/lib/netband/measurements/`. It rotates daily at UTC midnight or before a batch
would exceed 64 MiB (`rotate_max_bytes = 67108864`), whichever comes first. Complete
batches stay together, so the size threshold is soft. systemd provisions both the
state root and measurements subdirectory with mode 0700.

Change `output_dir` and `rotate_max_bytes` in `/etc/netband/netband.toml` to choose the
location and size threshold. Remove `rotate_max_bytes` for daily-only rotation. To use
one fixed file, replace `output_dir` with `output` and remove `rotate_max_bytes`.
Rotation takes place inside the running process and preserves scheduler accounting.

With `DynamicUser=yes`, systemd protects the state tree under `/var/lib/private` and
exposes it through `/var/lib/netband`. Use `sudo` to inspect it:

```sh
sudo ls -lah /var/lib/netband/measurements/
sudo journalctl -u netband.service -f
sudo journalctl -u netband.service -n 100 --no-pager
sudo journalctl -u netband.service -b
```

The operational log reports each new segment path. `.netband-active` in the measurement
directory records the current CSV basename. Following one CSV with `tail -f` does not
switch to new segments; use a [CSV reader](data-format.md#rotating-directory-output)
across the directory for analysis. Press `Ctrl-C` to stop following logs, or `q` to exit
the journal pager.

All segments are retained. Monitor available disk space and journal errors, and archive
closed segments under an explicit operator policy. Do not remove the segment named in
`.netband-active`, any `.netband-*` control files, or scheduler state and accounting.
The recorded segment is needed for restart recovery even after the service stops.
Do not use `copytruncate` or rename an active CSV. A size threshold does not bound total
disk usage, and the separate scheduler accounting ledger also grows over time.

Each ping round emits one row per target. Three targets at a five-second interval
produce 51,840 ping rows per day, plus bandwidth, request-failure, and scheduler
records. Storage also depends on identifiers and diagnostics. Measure your actual
files, choose an archive policy and free-space reserve, and alert on disk-full failures.

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

An interrupted bandwidth attempt records `cancelled` or `timeout` and retains any
completed download/upload measurements. An unfinished direction remains empty. These
measurement outcomes are separate from the service process exit code; see
[CSV outcomes](data-format.md#outcomes).

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

Only one process may write a CSV measurement journal (explicit or automatically named)
or own a scheduler state. CSV locks are held on the open files, separate from
systemd journal storage and the scheduler lock file. Stop duplicate
units/manual runs. Inspect the unit with `systemctl status` and directory permissions with
`sudo ls -ld /var/lib/netband/`; do not
delete lock files while a process is running. The CSV header must match the
[documented schema](data-format.md). If it does not, choose a new output file and
preserve the existing journal; do not manually edit its header.

**Bandwidth never starts**

`config check` reports `bandwidth.automatic_enabled`. M-Lab requires explicit policy
acceptance; `daily_max=0` and `--no-bandwidth` disable starts. Inspect scheduler events
in CSV and operational logs for daily cap, minimum spacing, pending-trigger TTL,
provider cooldown, or five-attempt expiry. Direct endpoints never fall back to M-Lab.

**No measurements in journald**

This is expected. The unit sends stdout to null. Inspect `/var/lib/netband/measurements/`
with a CSV reader; journald contains operational diagnostics only.

## State recovery

Scheduler state uses internal format 1, independent of CSV schema 1. The recovery set
contains:

| File | Purpose |
| --- | --- |
| `scheduler.json` | Current scheduler snapshot |
| `scheduler.bak` | Previous snapshot, when available |
| `scheduler.accounting.jsonl` | Sequenced, checksummed accounting records for all providers |
| `scheduler.initialized` | Installation identity and committed accounting checkpoint |
| `scheduler.lock` | Exclusive scheduler ownership |

Reservations and cooldown changes are synced to the accounting log and checkpoint
before snapshot replacement. A reservation is granted only after persistence succeeds.
Restoring an older snapshot replays newer accounting, preserving consumed starts,
last-start spacing, and provider cooldowns. Complete records beyond the checkpoint
are retained conservatively after an interrupted commit; an incomplete or inconsistent
log blocks bandwidth admission. Any persistence error requires reopening the scheduler
before further admission.

To recover a missing or corrupt primary snapshot:

1. Stop `netband.service` and confirm no process owns `scheduler.lock`.
2. Preserve the entire state directory, including temporary files, before making
   changes.
3. If a backup is available, copy `scheduler.bak` to `scheduler.json`. Leave the
   accounting log and checkpoint untouched. Valid JSON alone does not establish a
   valid recovery set; Netband checks installation identity, sequence, digests, and
   snapshot accounting when opening it.
4. Restart the service. Netband validates the recovery set under the state lock before
   updating it or admitting traffic. Review `journalctl -u netband.service` for errors.

If validation fails, retain the files and keep bandwidth disabled until a complete
recovery set is available. Do not delete the checkpoint, trim accounting records, or
hand-edit consumed allowances. Waiting for the next UTC day does not repair missing
or invalid evidence. Ping-only monitoring can continue with `--no-bandwidth`.

Interrupted initialization resumes automatically when the retained files establish
that only the initial, zero-accounting state exists. An incomplete log is rejected.
For a failed first installation known never to have admitted traffic, preserve and
move aside its state directory before retrying initialization. This is not a recovery
procedure for an installation that has already run bandwidth tests.

These guarantees depend on local filesystem locking, atomic replacement, and file and
directory syncing. They do not establish shared/network-filesystem support, recover a
provider response that was never durably recorded, or detect replacement of every file
with a mutually consistent older recovery set. The accounting log grows as accounting
changes; CSV rotation does not rotate it.

## Raspberry Pi

Use a 64-bit Raspberry Pi Linux image (`aarch64-unknown-linux-gnu`) and the pre-built
binary above, or follow [Build from source](#build-from-source). For hardware
validation, run [the release smoke checks](release.md) on real Pi hardware; CI's QEMU
aarch64 execution validates architecture/startup compatibility but not the board's
kernel, interfaces, capabilities, thermals, or sustained NDT7 behavior.
