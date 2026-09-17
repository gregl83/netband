# CSV schema and outcomes

Netband's v1 journal has 64 fields shared by CSV and JSONL. CSV is the authoritative
persisted output; JSONL emits the same records to the console. Each record represents
a run lifecycle transition, ping attempt, bandwidth attempt, request failure, or scheduler decision.

## Fields and encoding

```csv
schema_version,event_id,event_kind,event_sequence,run_id,parent_run_id,run_kind,scheduled_at_utc,started_at_utc,finished_at_utc,elapsed_ms,outcome,message,command,netband_version,process_id,interface,connection_details,provider_id,provider_kind,server_name,scheduler_action,trigger_reason,scheduler_not_before_utc,provider_daily_starts,ping_target_ip,ping_local_ip,ping_sequence,ping_packets_sent,ping_packets_received,ping_rtt_ms,ping_icmp_type,ping_icmp_code,load_run_id,load_phase,request_id,request_direction,request_stage,request_url,request_local_ip,request_remote_ip,request_http_status,request_retry_after_ms,request_retry_at_utc,download_request_id,download_local_ip,download_remote_ip,download_bytes,download_measurement_duration_ms,download_mbps,download_server_tcp_min_rtt_ms,download_server_tcp_rtt_ms,download_server_tcp_retransmitted_bytes,upload_request_id,upload_local_ip,upload_remote_ip,upload_bytes,upload_measurement_duration_ms,upload_mbps,upload_server_tcp_min_rtt_ms,upload_server_tcp_rtt_ms,upload_server_tcp_retransmitted_bytes,error_kind,os_error_code
```

Empty fields mean the value does not apply or was unavailable. Timestamps are RFC 3339
UTC with millisecond precision. Durations and RTTs are milliseconds. Throughput is
decimal megabits per second (`bytes * 8 / active_window_seconds / 1,000,000`).

JSONL uses the same fields and types, with JSON null instead of empty CSV cells.
Consumers must tolerate unknown JSON properties and future enum values without
misclassifying them as success; select known event kinds/outcomes explicitly.
Integers are decimal counters (some are unsigned 64-bit); readers must preserve their
precision. Measurement floats must be finite. A zero-duration window has no rate;
a direction that yields no measurement has unavailable duration, bytes and rate.
These are distinct from an actual measured zero. Human output is for display, not
parsing. Reject unsupported schema versions before interpreting measurements.

Schema version `1` identifies this contract independently of the application version.
Changes to CSV columns, order, types, units, or field meanings require a new schema
version. Optional connection metadata follows the extension rules below.

Columns use the following contiguous families, in the same order in CSV and JSONL.
Download and upload families have identical suffixes and ordering.

### Event and run identity

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer schema version, currently `1` |
| `event_id` | UUID identifying one event, independent of its run |
| `event_kind` | `run_started`, `run_finished`, `ping_probe`, `bandwidth`, `request_failure`, or `scheduler` |
| `event_sequence` | One-based journal emission order across a session and all its children; resets for each session |
| `run_id` | UUID of the session, ping round, or bandwidth attempt owning this event |
| `parent_run_id` | Parent session UUID on every child-run event; empty for session events |
| `run_kind` | `session`, `ping_round`, or `bandwidth`, populated on every event |

### Timing

| Field | Meaning |
| --- | --- |
| `scheduled_at_utc` | Planned slot time for scheduled bandwidth, trigger/opportunity creation time for triggered/manual bandwidth, or ping-round dispatch time; see timing semantics below |
| `started_at_utc` | Operation start time; request failures use stage start, lifecycle records use run start |
| `finished_at_utc` | Event completion time; empty on `run_started` |
| `elapsed_ms` | Monotonic elapsed time in milliseconds: full ping/bandwidth attempt or failed request stage; populated on those events even without a measurement, also populated on `run_finished`; empty on `run_started` and scheduler events |

### Result and explanation

| Field | Meaning |
| --- | --- |
| `outcome` | Classified result listed below |
| `message` | Sanitized human explanation of a decision or failure; optional, not machine-readable, and its presence alone does not indicate an error |

### Session provenance

| Field | Meaning |
| --- | --- |
| `command` | `run`, `once ping`, or `once bandwidth`; populated only on the session's `run_started` event |
| `netband_version` | Executable package version; populated only on the session's `run_started` event |
| `process_id` | Operating-system PID; populated only on the session's `run_started` event |

### Shared network context

| Field | Meaning |
| --- | --- |
| `interface` | Selected Linux interface; empty for default-route measurements or events without interface context |
| `connection_details` | Optional JSON object describing connection context; unavailable until supplied by a collector |

### Provider context

| Field | Meaning |
| --- | --- |
| `provider_id` | Persisted provider identity (`mlab` or hashed direct endpoint identity) |
| `provider_kind` | `mlab` or `direct` |
| `server_name` | Logical measurement-server identity: M-Lab machine name, or direct TLS name/download hostname; not necessarily the requested host |

### Scheduling and accounting

| Field | Meaning |
| --- | --- |
| `scheduler_action` | Structured scheduling decision; values listed under Diagnostics |
| `trigger_reason` | `scheduled`, `ping_loss`, `ping_rtt`, or `manual` |
| `scheduler_not_before_utc` | Deadline applied by this scheduler decision, including provider cooldown, minimum spacing or interface retry; other policy gates may still block work after this time; empty on other event kinds |
| `provider_daily_starts` | Bandwidth starts reserved for this provider and UTC day, including failed or interrupted attempts; not a count of successful tests |

### Ping

| Field | Meaning |
| --- | --- |
| `ping_target_ip` | Configured ping target IP address; distinct from an actually established remote endpoint |
| `ping_local_ip` | Netband’s local address actually bound/used for the ping; empty when unavailable and on other event kinds |
| `ping_sequence` | ICMP sequence number |
| `ping_packets_sent` | Ping-only: 1 if the probe was sent, otherwise 0 |
| `ping_packets_received` | Ping-only: 1 if a successful reply was received, otherwise 0 |
| `ping_rtt_ms` | Successful ICMP round-trip time in milliseconds |
| `ping_icmp_type` | Returned ICMP type when available |
| `ping_icmp_code` | Returned ICMP code when available |

### Concurrent load

| Field | Meaning |
| --- | --- |
| `load_run_id` | `run_id` of the concurrent bandwidth attempt; empty without a concurrent test |
| `load_phase` | Concurrent NDT7 phase at ping-round start: `setup`, `download`, or `upload`; empty without a concurrent test |

### Request

| Field | Meaning |
| --- | --- |
| `request_id` | Opaque string identifying the failed request attempt across stages; use equality for correlation |
| `request_direction` | `download` or `upload` on NDT7 request failures, including setup, transfer, and cleanup; null/empty for shared discovery or admission failures and all other event kinds |
| `request_stage` | `locate`, `dns`, `connect`, `tls`, `websocket_handshake`, `download`, or `upload` |
| `request_url` | Sanitized endpoint URL on request-failure events, including scheme, host, port and path; credentials/fragments removed and query redacted; empty on bandwidth summaries |
| `request_local_ip` | Local address of a failed request when known; empty on other event kinds |
| `request_remote_ip` | Actual remote address of a failed request when known; empty on other event kinds |
| `request_http_status` | HTTP/WebSocket handshake status when returned |
| `request_retry_after_ms` | Parsed provider delay at response receipt; HTTP-dates report remaining time, floored at zero; empty if absent, invalid, or too large for this field |
| `request_retry_at_utc` | Provider Retry-After deadline captured at response receipt; populated only on request failures when the header is usable |

### Download

| Field | Meaning |
| --- | --- |
| `download_request_id` | Request ID of the retained download measurement; empty without that measurement |
| `download_local_ip` | Local address of the download connection; empty when that direction is unavailable |
| `download_remote_ip` | Remote address of the download connection; empty when that direction is unavailable |
| `download_bytes` | Application payload bytes received |
| `download_measurement_duration_ms` | Client receive measurement window in milliseconds; empty when that direction is unavailable |
| `download_mbps` | NDT7 download throughput in decimal Mb/s |
| `download_server_tcp_min_rtt_ms` | NDT7 server TCPInfo minimum RTT in milliseconds for the download connection |
| `download_server_tcp_rtt_ms` | NDT7 server TCPInfo current/smoothed RTT in milliseconds for the download connection |
| `download_server_tcp_retransmitted_bytes` | NDT7 server TCPInfo retransmitted bytes (`BytesRetrans`) for the download connection |

### Upload

| Field | Meaning |
| --- | --- |
| `upload_request_id` | Request ID of the retained upload measurement; empty without that measurement |
| `upload_local_ip` | Local address of the upload connection; empty when that direction is unavailable |
| `upload_remote_ip` | Remote address of the upload connection; empty when that direction is unavailable |
| `upload_bytes` | Binary application payload bytes accepted by the WebSocket sink during active upload; includes any buffered tail, excludes WebSocket and TLS overhead |
| `upload_measurement_duration_ms` | Client send measurement window in milliseconds; excludes close-handshake waiting; empty when that direction is unavailable |
| `upload_mbps` | NDT7 upload throughput in decimal Mb/s |
| `upload_server_tcp_min_rtt_ms` | NDT7 server TCPInfo minimum RTT in milliseconds for the upload connection |
| `upload_server_tcp_rtt_ms` | NDT7 server TCPInfo current/smoothed RTT in milliseconds for the upload connection |
| `upload_server_tcp_retransmitted_bytes` | NDT7 server TCPInfo retransmitted bytes (`BytesRetrans`) for the upload connection |

### Error details

| Field | Meaning |
| --- | --- |
| `error_kind` | Stable machine-readable failure classification |
| `os_error_code` | Operating-system error number when available |


## Event context and relationships

| Event kind | Record scope | Field context |
| --- | --- | --- |
| `run_started` | Start of a session or child run | Run identity, parent, kind, start timestamp; session starts also include command, version and PID; outcome is `started` |
| `run_finished` | Completion or orderly termination of that run | Same run identity, parent and kind; start/finish timestamps, monotonic elapsed time, outcome and any command failure |
| `ping_probe` | One attempt against one target | Target, sequence, packet counts, RTT, ICMP details, and any failure; load fields identify a concurrent bandwidth attempt |
| `bandwidth` | One bandwidth attempt | Provider, logical server, trigger, accounting, and independently available download/upload measurements |
| `request_failure` | One failed request or stage within a bandwidth attempt | Request URL, direction, stage, request ID, known endpoints, response details, and diagnostic |
| `scheduler` | One scheduling decision | Provider, trigger, accounting, applicable cooldown, and decision explanation; measurement fields are unavailable |

Each ping attempt produces one complete `ping_probe`, including failures. Packet
counts describe that attempt; rolling health calculations remain internal. Calculate
aggregate loss from probe rows as
`100 * (SUM(ping_packets_sent) - SUM(ping_packets_received)) / SUM(ping_packets_sent)`.
Loss is unavailable when the denominator is zero. Unsent probes remain visible as
failed attempts but do not contribute to the packet-loss denominator. Human output
may display derived loss; no redundant per-probe loss percentage is stored.

Every measurement command creates a root session. Each ping round and bandwidth
attempt has a distinct child run, including measurements from one-shot commands.
Scheduler decisions belong to the session. `config check` emits no journal events.
A child start is durably recorded before its measurement work begins, and a session
start precedes every child start. Results precede their run's finish record.

Join children to the session start using `parent_run_id = run_id`. Join probe events
by `run_id` to group a ping round; ICMP `ping_sequence` wraps and is not a round identifier.
Join request failures to their bandwidth result by `run_id`, and use `event_id` for
deduplication. `load_run_id` separately links a probe to overlapping bandwidth work.

Run, event and request IDs are UUID v4 values serialized as canonical lowercase,
hyphenated strings. Treat IDs as opaque equality keys, not timestamps or counters.
UUID generation requires operating-system randomness; failure does not fall back to
weaker identifiers. `event_sequence` provides emission order across a session and its
concurrent children, even when operation timestamps tie or move backward. It is not
operation-start order, and gaps may indicate omitted output. Console JSONL can drop
records under backpressure; CSV remains authoritative.

A run finish records success, partial results, failure or cancellation. A monitor
session ends with `cancelled` on orderly shutdown; drained ping rounds retain their
measurement outcome. Missing finish records mean completion was not recorded, not
necessarily that the process crashed. No synthetic finish is written on process exit.
Preflight configuration/storage failures can prevent a session from starting at all.

Use `request_direction` together with `request_stage` for failure attribution. For example, `upload` plus `tls` means upload TLS setup failed. Direction is selected
before DNS and retained through timeout/cancellation and cleanup; it does not depend
on URL path naming or whether a measurement was retained. Shared Locate discovery
and pre-connection admission failures leave direction unavailable/not applicable.
Do not infer direction from diagnostic prose or count each request as a bandwidth test.
Rate availability and diagnostics must be assessed independently of overall outcome.

`request_id` is an opaque string identifying one request attempt. Endpoint
resolution and admission share an ID, including Locate discovery, redirects, and
candidate validation. Direct configuration resolution also has an ID; its presence
does not imply a network connection was established. Each direction receives a new
ID before DNS. The first address retains that ID; each subsequent address attempt
receives a fresh ID. Different directions, server candidates, and runs receive fresh
IDs. TLS, handshake, transfer, cleanup, timeout, and cancellation retain the active ID.

`download_request_id` and `upload_request_id` reference the requests that produced
retained measurements. Match these to failure records' `request_id` within the same
`run_id`. A measurement and cleanup failure can share an ID. Missing result IDs mean
no measurement was retained, not that no request occurred. These IDs do not encode
direction, retry count, or order; do not parse or sort their representation. Count
distinct IDs across failure records and retained results to count represented attempts
for a direction. Timestamps provide a timeline but can tie or move backward.

During automatic bandwidth tests in `run`, ping rounds continue on the selected bandwidth
interface. A ping is under load when `load_phase` is `download` or `upload`; `setup`
covers discovery and connection work that does not itself represent throughput load.
The upload phase includes bounded cleanup because buffered traffic can still drain.
Load-classified pings remain durable measurements but are excluded from the health window
that can request another bandwidth test. Join `load_run_id` to the bandwidth row's
`run_id` when analyzing loaded latency. Because rows are committed as operations finish,
use their timestamps rather than file order when constructing a timeline.

## Timing and throughput

`scheduled_at_utc` records the opportunity's origin, whose meaning depends on context:

- Scheduled bandwidth: the planned slot time.
- Health-triggered bandwidth: the triggering opportunity's creation time.
- Manual bandwidth: the manual opportunity's creation time.
- Ping: the round's dispatch time, not the nominal ticker deadline.

It is unavailable on lifecycle/scheduler rows and on measurement failures that were
not annotated by completed scheduler handling. It is not a universal scheduling-delay
baseline. `started_at_utc` and `finished_at_utc` retain actual operation boundaries.

`request_retry_after_ms` and `request_retry_at_utc` preserve a response's delay and
absolute deadline. `scheduler_not_before_utc` records the deadline applied by the
scheduler, which can retain a later existing cooldown or represent local spacing or
interface backoff. Do not substitute one deadline for the other.

Bandwidth `started_at_utc` is captured before endpoint resolution, and
`finished_at_utc` when the attempt terminates, including setup and upload cleanup.
Both are recorded even when the attempt fails, times out, or is cancelled before
producing measurements. Request-failure rows retain the stage start and failure time.
UTC timestamps reflect the observed wall clock and can move backward after a clock
adjustment. Elapsed time and active durations use a monotonic clock.

`download_measurement_duration_ms` and `upload_measurement_duration_ms` retain each active monotonic window.
Recompute download as `8 * download_bytes / (1000 * download_measurement_duration_ms)` and upload
as `8 * upload_bytes / (1000 * upload_measurement_duration_ms)` in decimal Mb/s. Allow floating-point
roundoff (relative tolerance `1e-12`); CSV and JSONL do not round measurements for display.
`elapsed_ms` measures the entire attempt independently, including discovery, connection
setup, transfer and cleanup. It is available even when both directions fail before
measurement. It is not the sum of the direction windows and is not used to calculate
throughput. On request-failure rows it measures that failed stage (Locate covers the
discovery operation, including redirects), not the whole bandwidth attempt. Timings
are captured when the operation finishes; delayed journaling does not extend them.
Scheduler decisions have no measured operation interval and leave `elapsed_ms` empty.
Do not derive elapsed time by subtracting UTC timestamps: those timestamps have
millisecond precision and can move backward when the system clock changes.

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
refund a reserved start.

## Server and connection identity

`server_name` and `request_url` serve different purposes. A TLS name can differ from
an IP-literal URL host; download and upload may use different endpoints. Do not derive
one field from the other. Request failures retain both when a candidate is known.
Locate failures normally have no measurement-server name; invalid candidate records
can carry the machine name and the Locate URL that returned them. `request_url` records
the configured Locate URL (not necessarily the final redirect URL) or the direction's
NDT7 URL. A bandwidth summary has a logical name but no single request URL.

Direction-specific addresses remain empty when that direction has no retained
measurement. `ping_local_ip`, `request_local_ip`, and `request_remote_ip` are empty on bandwidth summaries;
use the direction-specific addresses even when both endpoints share a hostname.
Local addresses always identify Netband’s endpoint and remote addresses the server’s endpoint,
regardless of which side sends the payload.

## TCP measurements

TCP fields identify their download or upload connection and remain empty when
unavailable for that direction. Values come from that direction's last parsed TCPInfo
snapshot, rather than a time series or a difference calculated by Netband. Both sets
are server-reported: upload retransmitted bytes describe the server's TCP connection
counters, not client-side upload retransmissions or a packet-loss ratio.

## Connection metadata

`connection_details` is an optional JSON object. JSONL embeds the object directly;
CSV stores compact JSON text in one normally escaped CSV cell. Unavailable details
are JSONL `null` and an empty CSV cell, not the strings `"null"` or `"{}"`. An explicit
empty object `{}` is valid but makes no claim that collection succeeded. Collectors
leave this field unavailable; Wi-Fi/system metadata collection is not implemented.

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

## Diagnostics

`error_kind` is empty/null without a classified error; its emitted values are:
`icmp_timeout`, `icmp_unreachable`, `permission_denied`, `dns`, `connect`, `tls`,
`http_status`, `websocket_handshake`, `download_failed`, `upload_failed`,
`provider_cooldown`, `daily_cap`, `cancelled`, `timeout`, `io`, `protocol`, `internal`.
`os_error_code` is platform-specific; `message` is sanitized explanatory prose
and is not a stable machine contract.

Scheduler decisions use `scheduler_action` and optional `message`. Normal cap,
spacing, cooldown and health decisions leave `error_kind` and `os_error_code` empty.
An actual interface resolution or clock error may additionally populate those fields.
The same `message` column holds any human-readable failure explanation.

| `scheduler_action` | Meaning |
| --- | --- |
| `trigger_pending` | Health degradation created a pending trigger |
| `trigger_merged_with_deferred` | Health trigger merged into an existing retry |
| `trigger_cancelled` | Health recovery cleared a pending trigger |
| `trigger_expired` | Trigger lifetime expired while retaining its health latch |
| `deferred_expired` | Retry reached its day or attempt limit |
| `bandwidth_start` | Scheduler admitted a bandwidth opportunity |
| `rate_limit` | Provider response established a cooldown/retry decision |
| `clock_rollback` | Backward wall-clock movement blocked scheduling |
| `suppressed` | Policy blocked an opportunity |
| `deferred` | Policy delayed an opportunity |
| `interface_recovered` | Interface became available again |
| `interface_retry` | Interface resolution failed and will be retried |
| `bandwidth_suppressed` | Interface selection prevented an attempt |
| `bandwidth_interface_skipped` | Unavailable interface was skipped during selection |

## Outcomes

| Outcome | Meaning |
| --- | --- |
| `started` | Run began; only on `run_started` |
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

Failures are data. A failed ping still produces one `ping_probe`; HTTP,
TLS, WebSocket, download, and upload failures produce sanitized `request_failure` rows.

## Journal storage

Existing CSV files are appended only when their header exactly matches the schema. On startup, an unterminated trailing record is
discarded and reported to the operational log; completed malformed records fail closed.
Each completed batch is flushed and synced.

Explicit and automatically named CSV files hold an exclusive OS file lock from before
header initialization or recovery until the file closes. A competing Netband writer
fails without changing the file. The OS releases the lock when the process exits,
including after a crash. Fixed-file output needs no separate lock file. On Linux the
lock is advisory: readers can inspect the CSV, and unrelated writers can ignore it.

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

## Reading examples

See the [synthetic JSONL examples](examples/README.md) for every event kind, failure
paths, partial measurements, and illustrative optional metadata.

Use an independent CSV implementation when ingesting journals. For example:

```sh
python3 - <<'PY'
import csv
with open("netband.csv", newline="", encoding="utf-8") as stream:
    rows = list(csv.DictReader(stream))
assert rows and "ping_probe" in {r["event_kind"] for r in rows}
print(f"parsed {len(rows)} rows with {len(rows[0])} fields")
PY
```
