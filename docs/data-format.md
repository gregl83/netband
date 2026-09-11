# CSV schema and outcomes

The CSV journal is Netband's authoritative output. The current first-release schema version 1 has 53 columns, one header and
one row per measurement or scheduler/request event. Existing files are appended only
when their header exactly matches. On startup, an unterminated trailing record is
discarded and reported to the operational log; completed malformed records fail closed.
Each completed batch is flushed and synced.

Explicit and automatically named CSV files hold an exclusive OS file lock from before
header initialization or recovery until the file closes. A competing Netband writer
fails without changing the file. The OS releases the lock when the process exits,
including after a crash. Fixed-file output needs no separate lock file. On Linux the
lock is advisory: readers can inspect the CSV, and unrelated writers can ignore it.

```csv
schema_version,run_id,event_id,scheduled_at_utc,started_at_utc,finished_at_utc,interface,local_ip,connection_details,event_kind,trigger_reason,load_phase,load_run_id,target,sequence,outcome,duration_ms,rtt_ms,packets_sent,packets_received,packet_loss_pct,icmp_type,icmp_code,provider_id,provider_kind,server_name,request_url,remote_ip,request_stage,request_attempt,http_status,retry_after_ms,rate_limit_until_utc,daily_bandwidth_starts,download_mbps,upload_mbps,upload_bytes,download_bytes,download_duration_ms,upload_duration_ms,download_local_ip,upload_local_ip,download_remote_ip,upload_remote_ip,download_tcp_min_rtt_ms,download_tcp_rtt_ms,download_tcp_retransmitted_bytes,upload_tcp_min_rtt_ms,upload_tcp_rtt_ms,upload_tcp_retransmitted_bytes,os_error_code,error_kind,error_message
```

Empty fields mean the value does not apply or was unavailable. Timestamps are RFC 3339
UTC with millisecond precision. Durations and RTTs are milliseconds. Throughput is
decimal megabits per second (`bytes * 8 / elapsed_seconds / 1,000,000`).

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer schema version, currently `1` |
| `run_id` | Identifier for a measurement stream; automatic bandwidth attempts use a nested run ID |
| `event_id` | Unique event identifier within the run |
| `scheduled_at_utc` | Planned opportunity or trigger creation time |
| `started_at_utc` | Attempt start time; request-failure rows record the request-stage start |
| `finished_at_utc` | Event completion time |
| `interface` | Selected Linux interface; empty means default route |
| `local_ip` | Netband’s local address actually bound/used for ping or request failures; empty on combined bandwidth rows |
| `connection_details` | Optional JSON object containing extensible connection metadata; see below |
| `event_kind` | `ping_probe`, `ping_summary`, `bandwidth`, `request_failure`, or `scheduler` |
| `trigger_reason` | `scheduled`, `ping_loss`, `ping_rtt`, or `manual` |
| `load_phase` | Concurrent NDT7 phase at ping-round start: `setup`, `download`, or `upload`; empty without a concurrent test |
| `load_run_id` | `run_id` of the concurrent bandwidth attempt; empty without a concurrent test |
| `target` | Ping target address |
| `sequence` | ICMP sequence number |
| `outcome` | Classified result listed below |
| `duration_ms` | Attempt duration; for bandwidth, sum of direction measurement windows, excluding setup and upload close-handshake waiting |
| `rtt_ms` | Successful ICMP round-trip time in milliseconds |
| `packets_sent` | Probe count represented by the row |
| `packets_received` | Successful reply count represented by the row |
| `packet_loss_pct` | Packet loss percentage from 0 through 100; empty when no packet was sent |
| `icmp_type` | Returned ICMP type when available |
| `icmp_code` | Returned ICMP code when available |
| `provider_id` | Persisted provider identity (`mlab` or hashed direct endpoint identity) |
| `provider_kind` | `mlab` or `direct` |
| `server_name` | Logical measurement-server identity: M-Lab machine name, or direct TLS name/download hostname; not necessarily the requested host |
| `request_url` | Sanitized endpoint URL on request-failure events, including scheme, host, port and path; credentials/fragments removed and query redacted; empty on bandwidth summaries |
| `remote_ip` | Actual remote address for request failures when known; empty on combined bandwidth rows |
| `request_stage` | `locate`, `dns`, `connect`, `tls`, `websocket_handshake`, `download`, or `upload` |
| `request_attempt` | One-based request/candidate attempt number |
| `http_status` | HTTP/WebSocket handshake status when returned |
| `retry_after_ms` | Parsed provider delay at response receipt; HTTP-dates report remaining time, floored at zero; empty if absent, invalid, or too large for this field |
| `rate_limit_until_utc` | Provider retry deadline on request-failure rows; enforced cooldown deadline on scheduler rows, which may retain a later existing cooldown |
| `daily_bandwidth_starts` | Bandwidth starts reserved for this provider and UTC day, including failed or interrupted attempts; not a count of successful tests |
| `download_mbps` | NDT7 download throughput in decimal Mb/s |
| `upload_mbps` | NDT7 upload throughput in decimal Mb/s |
| `upload_bytes` | Binary application payload bytes accepted by the WebSocket sink during active upload; includes any buffered tail, excludes WebSocket and TLS overhead |
| `download_bytes` | Application payload bytes received |
| `download_duration_ms` | Client receive measurement window in milliseconds; empty when that direction is unavailable |
| `upload_duration_ms` | Client send measurement window in milliseconds; excludes close-handshake waiting; empty when that direction is unavailable |
| `download_local_ip` | Local address of the download connection; empty when that direction is unavailable |
| `upload_local_ip` | Local address of the upload connection; empty when that direction is unavailable |
| `download_remote_ip` | Remote address of the download connection; empty when that direction is unavailable |
| `upload_remote_ip` | Remote address of the upload connection; empty when that direction is unavailable |
| `download_tcp_min_rtt_ms` | NDT7 server TCPInfo minimum RTT in milliseconds for the download connection |
| `download_tcp_rtt_ms` | NDT7 server TCPInfo current/smoothed RTT in milliseconds for the download connection |
| `download_tcp_retransmitted_bytes` | NDT7 server TCPInfo retransmitted bytes (`BytesRetrans`) for the download connection |
| `upload_tcp_min_rtt_ms` | NDT7 server TCPInfo minimum RTT in milliseconds for the upload connection |
| `upload_tcp_rtt_ms` | NDT7 server TCPInfo current/smoothed RTT in milliseconds for the upload connection |
| `upload_tcp_retransmitted_bytes` | NDT7 server TCPInfo retransmitted bytes (`BytesRetrans`) for the upload connection |
| `os_error_code` | Operating-system error number when available |
| `error_kind` | Stable machine-readable failure classification |
| `error_message` | Sanitized human diagnostic; wording may change in future releases |

`server_name` and `request_url` serve different purposes. A TLS name can differ from
an IP-literal URL host; download and upload may use different endpoints. Do not derive
one field from the other. Request failures retain both when a candidate is known.
Locate failures normally have no measurement-server name; invalid candidate records
can carry the machine name and the Locate URL that returned them. `request_url` records
the configured Locate URL (not necessarily the final redirect URL) or the direction's
NDT7 URL. A bandwidth summary has a logical name but no single request URL.

## Connection details and schema evolution

`connection_details` is an optional JSON object. JSONL embeds the object directly;
CSV stores compact JSON text in one normally escaped CSV cell. Unavailable details
are JSONL `null` and an empty CSV cell, not the strings `"null"` or `"{}"`. An explicit
empty object `{}` is valid but makes no claim that collection succeeded. Current
collectors leave this field unavailable. No Wi-Fi/system metadata collection is
introduced by the schema field.

A future producer could emit this object (illustrative, not currently collected):

```json
{"wifi":{"signal_dbm":-62,"frequency_mhz":5180,"band":"5GHz"}}
```

Readers must ignore unknown properties at every nesting level. Producers may add
optional properties without a schema-version change. Once a property is introduced,
its name, type, units, and meaning are stable; incompatible changes require a new
property name or a schema version. Missing properties and explicit JSON null mean
unavailable/not applicable; preserve numeric zero and boolean false. Group related
properties in objects such as `wifi`, and document each new property's units and
sampling time when its collector is introduced. Object key order is not meaningful.
Details describe the event's connection context; future per-direction details must
identify their direction rather than imply two connections are identical.

Metadata producers must select documented fields, exclude credentials and tokens,
and sanitize any endpoint values before inserting them. The serializer preserves
object values; it does not sanitize arbitrary nested metadata. Interface names,
addresses, network identifiers and timestamps can identify a host or network. Share
a reviewed copy while retaining the original journal as evidence.

Schema versions are independent of application versions. This is the first supported
release format and retains `schema_version=1`. Pre-release journals with earlier
headers are not compatible: preserve them and select a new file/output directory;
do not rewrite their headers. The writer requires an exact header to append.
After this freeze, changes to CSV columns, order, types, units or field meanings
require a schema-version change and a new journal or explicit migration. Optional
connection-detail properties follow the additive rule above.

JSONL uses the same fields and types, with JSON null instead of empty CSV cells.
Consumers must tolerate unknown JSON properties and future enum values without
misclassifying them as success; select known event kinds/outcomes explicitly.
Integers are decimal counters (some are unsigned 64-bit); readers must preserve their
precision. Measurement floats must be finite. A zero-duration window has no rate;
a direction that yields no measurement has unavailable duration, bytes and rate.
These are distinct from an actual measured zero. Human output is for display, not
parsing. Reject unsupported schema versions before interpreting measurements.

Join request failures to their bandwidth summary by `run_id` (and use `event_id` for
deduplication). An automatic attempt has its own nested `run_id`; use `load_run_id`
from ping events to join it. Use `request_stage` for failure-stage attribution;
early setup failures do not necessarily identify a direction. Do not infer missing
direction attribution from diagnostic prose or count each request as a bandwidth test.
Rate availability and diagnostics must be assessed independently of overall outcome.

`error_kind` is empty/null without a classified error; its emitted values are:
`icmp_timeout`, `icmp_unreachable`, `permission_denied`, `dns`, `connect`, `tls`,
`http_status`, `websocket_handshake`, `download_failed`, `upload_failed`,
`provider_cooldown`, `daily_cap`, `cancelled`, `timeout`, `io`, `protocol`, `internal`.
`os_error_code` is platform-specific; `error_message` is sanitized explanatory prose
and is not a stable machine contract.

## Rotating directory output

`--output FILE` appends to one CSV across restarts and never rotates.
`--output-dir DIR` (or the current directory when neither option is supplied) creates
segments named `netband-YYYYMMDDTHHMMSS.sssZ.csv`. A numeric suffix resolves collisions
without overwriting files. Every startup creates a fresh segment.

Directory output rotates during collection at the first write on a later UTC date.
An optional `--rotate-max-bytes BYTES` also rotates before the next batch would exceed
that size, counting the header and actual encoded bytes. Each batch stays together;
a batch larger than the limit is written to one segment. Empty batches do not rotate,
and idle days do not create intervening files. A backward clock adjustment does not
reopen older segments or reset the running writer's date boundary.

Segment selection uses journal write time. Event timestamps remain unchanged, so a
measurement spanning midnight can have records in different segments. Group records
by `run_id` and `event_id`, and parse each CSV separately to account for its header.
Filenames do not establish measurement order after wall-clock adjustments.

Rotation syncs the previous segment and initializes a complete, synced header before
publishing the next CSV. A hard link publishes the staged file without overwriting an
existing name. Unix builds sync directory metadata before committing events. This
requires a local filesystem supporting file locks, hard links, and the relevant sync
operations; it does not establish durability on arbitrary network filesystems or
against hardware that ignores sync requests. Rotation does not restart scheduling,
but disk I/O can delay commits. Creation, locking, write, and sync failures are fatal.

Directory mode holds `.netband-output.lock` for the process lifetime. Only one Netband
directory writer may use that directory. `.netband-active` records the current CSV
basename and is atomically replaced and synced before that segment receives events.
On restart, Netband validates and recovers only that recorded segment before starting
a new file. Missing segments, invalid markers/headers, and malformed complete rows
fail closed; an incomplete final row is discarded with a diagnostic, as in fixed-file
mode. A competing explicit-file writer prevents recovery while it holds the CSV lock.
Historical CSVs are not scanned or modified.

Do not delete the lock or marker files, or move the recorded segment independently.
The lock file can remain after exit: the OS lock is released automatically. Temporary
`.netband-header.tmp` and `.netband-active.tmp` files can remain after a failure and are
reused or replaced on the next successful startup. A failure before the first event
can leave a valid header-only CSV. Preserve damaged recovery evidence for inspection;
restore the recorded segment from a trusted copy or start in a fresh directory after
preserving the old directory. Do not bypass corruption by deleting the marker.

Acknowledgement follows batch sync. A crash can leave a complete but unacknowledged
row; automatic replay of uncertain batches is not provided. Closed segments are never
automatically compressed or deleted. Rotation limits individual segment growth subject
to the batch exception; it does not bound total storage. Keep scheduler accounting and
scientific dataset manifests/checksums separate from any operator archive cleanup.

This changes the earlier directory behavior of one file per process start and permits
only one directory writer per directory. Fixed-file rotation behavior is unchanged. Existing timestamped CSVs without a marker are retained untouched;
validate them independently before treating them as complete archives.

## Bandwidth field limitations

Bandwidth `started_at_utc` is captured before endpoint resolution, and
`finished_at_utc` when the attempt terminates, including setup and upload cleanup.
Both are recorded even when the attempt fails, times out, or is cancelled before
producing measurements. Request-failure rows retain the stage start and failure time.
UTC timestamps reflect the observed wall clock and can move backward after a clock
adjustment. Throughput and active durations use monotonic elapsed time.

`download_duration_ms` and `upload_duration_ms` retain each active monotonic window.
Recompute download as `8 * download_bytes / (1000 * download_duration_ms)` and upload
as `8 * upload_bytes / (1000 * upload_duration_ms)` in decimal Mb/s. Allow floating-point
roundoff (relative tolerance `1e-12`); CSV and JSONL do not round measurements for display.
`duration_ms` is their sum, or unavailable when neither direction was measured.
The six direction-specific duration/address fields remain empty for an unavailable
direction. Generic `local_ip` and `remote_ip` are empty on bandwidth summaries;
use the direction-specific addresses even when both endpoints share a hostname.
`local_ip` always means Netband’s endpoint and `remote_ip` the server’s endpoint,
regardless of which side sends the payload.
TCP fields identify their
download or upload connection and remain empty when unavailable for that direction.
Values come from that direction's last parsed TCPInfo snapshot, rather than a time series
or a difference calculated by Netband. Both sets are server-reported: upload retransmitted bytes describe the server's TCP
connection counters, not client-side upload retransmissions or a packet-loss ratio.

## Outcomes

| Outcome | Meaning |
| --- | --- |
| `success` | Requested operation completed |
| `partial` | One bandwidth direction is available without an overriding timeout, cancellation, or provider-wide failure |
| `timeout` | Configured operation deadline expired |
| `unreachable` | ICMP/network unreachable response |
| `permission_denied` | Host denied the required socket or file operation |
| `cancelled` | Shutdown cancelled the active operation |
| `error` | Classified failure not represented by another outcome |
| `no_capacity` | Provider reported no usable server/capacity |
| `rate_limited` | Provider requested traffic reduction |
| `scheduled` | Scheduler admitted an opportunity |
| `rescheduled` | Remaining opportunities were recalculated |
| `deferred` | Opportunity retained for later eligibility |
| `suppressed` | Cap, spacing, cooldown, or policy permanently blocked this opportunity |
| `expired` | Pending opportunity exceeded its lifetime/attempt limit |

Failures are data. A failed ping still produces a `ping_probe` and `ping_summary`; HTTP,
TLS, WebSocket, download, and upload failures produce sanitized `request_failure` rows.

NDT7 upload accepts new payloads for at most ten seconds after its handshake, or until
the peer closes or a transport error occurs. Its rate uses locally accepted bytes and
that active window, not confirmed server receipt. An accepted final payload may still
drain during cleanup; neither those bytes nor pure close-handshake waiting are counted
again. Upload cleanup has a separate two-second limit, subject to earlier cancellation
or the whole-test timeout. A normal active-window deadline is not an error. A stalled
close handshake produces an `upload_failed` diagnostic with
`upload close handshake timed out after 2s`; other transport errors retain their details.
Collected direction measurements remain available even when cleanup fails, so a
bandwidth `success` means both rates are available, not that transport shutdown was clean.

If the whole-test deadline or shutdown interrupts an attempt, its bandwidth row keeps
`timeout` or `cancelled` as the outcome and retains all completed direction measurements.
For example, a completed download remains available when upload setup or transfer is
interrupted. Upload bytes, rate, and measurement duration are retained once its active
window ends, even if cleanup is interrupted. An unfinished direction is left empty,
not reported as zero throughput. Earlier request diagnostics are preserved, and the
terminal `request_failure` identifies the interrupted stage.

Use field availability alongside `outcome` when analyzing these rows; filtering only
for `success` discards usable measurements from interrupted attempts. Retained rates
keep their normal observation points and exclude cleanup time. Interruption does not
refund a reserved start or change the CSV schema.

During automatic bandwidth tests in `run`, ping rounds continue on the selected bandwidth
interface. A ping is under load when `load_phase` is `download` or `upload`; `setup`
covers discovery and connection work that does not itself represent throughput load.
The upload phase includes bounded cleanup because buffered traffic can still drain.
Load-classified pings remain durable measurements but are excluded from the health window
that can request another bandwidth test. Join `load_run_id` to the bandwidth row's
`run_id` when analyzing loaded latency. Because rows are committed as operations finish,
use their timestamps rather than file order when constructing a timeline.

Use an independent CSV implementation when ingesting journals. For example:

```sh
python3 - <<'PY'
import csv
with open("netband.csv", newline="", encoding="utf-8") as stream:
    rows = list(csv.DictReader(stream))
assert rows and {"ping_probe", "ping_summary"} <= {r["event_kind"] for r in rows}
print(f"parsed {len(rows)} rows with {len(rows[0])} fields")
PY
```
