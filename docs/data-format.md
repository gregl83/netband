# CSV schema and outcomes

The CSV journal is Netband's authoritative output. Schema version 1 has one header and
one row per measurement or scheduler/request event. Existing files are appended only
when their header exactly matches. On startup, an unterminated trailing record is
discarded and reported to the operational log; completed malformed records fail closed.
Each completed batch is flushed and synced.

```csv
schema_version,run_id,event_id,scheduled_at_utc,started_at_utc,finished_at_utc,interface,source_ip,event_kind,trigger_reason,load_phase,load_run_id,target,sequence,outcome,duration_ms,rtt_ms,packets_sent,packets_received,packet_loss_pct,icmp_type,icmp_code,provider_id,provider_kind,server,remote_ip,request_stage,request_attempt,http_status,retry_after_ms,rate_limit_until_utc,daily_runs_used,download_mbps,upload_mbps,bytes_sent,bytes_received,tcp_min_rtt_ms,tcp_rtt_ms,tcp_retransmissions,os_error_code,error_kind,error_message
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
| `started_at_utc` | Actual attempt start time |
| `finished_at_utc` | Event completion time |
| `interface` | Selected Linux interface; empty means default route |
| `source_ip` | Source address actually bound/used when known |
| `event_kind` | `ping_probe`, `ping_summary`, `bandwidth`, `request_failure`, or `scheduler` |
| `trigger_reason` | `scheduled`, `ping_loss`, `ping_rtt`, or `manual` |
| `load_phase` | Concurrent NDT7 phase at ping-round start: `setup`, `download`, or `upload`; empty without a concurrent test |
| `load_run_id` | `run_id` of the concurrent bandwidth attempt; empty without a concurrent test |
| `target` | Ping target address |
| `sequence` | ICMP sequence number |
| `outcome` | Classified result listed below |
| `duration_ms` | Whole attempt or test duration in milliseconds |
| `rtt_ms` | Successful ICMP round-trip time in milliseconds |
| `packets_sent` | Probe count represented by the row |
| `packets_received` | Successful reply count represented by the row |
| `packet_loss_pct` | Packet loss percentage from 0 through 100 |
| `icmp_type` | Returned ICMP type when available |
| `icmp_code` | Returned ICMP code when available |
| `provider_id` | Persisted provider identity (`mlab` or hashed direct endpoint identity) |
| `provider_kind` | `mlab` or `direct` |
| `server` | Sanitized logical Locate/NDT endpoint; query values are redacted |
| `remote_ip` | Actual remote measurement address when known |
| `request_stage` | `locate`, `dns`, `connect`, `tls`, `websocket_handshake`, `download`, or `upload` |
| `request_attempt` | One-based request/candidate attempt number |
| `http_status` | HTTP/WebSocket handshake status when returned |
| `retry_after_ms` | Provider `Retry-After` delay in milliseconds |
| `rate_limit_until_utc` | Persisted provider cooldown deadline |
| `daily_runs_used` | Reserved starts for this provider and UTC day |
| `download_mbps` | NDT7 download throughput in decimal Mb/s |
| `upload_mbps` | NDT7 upload throughput in decimal Mb/s |
| `bytes_sent` | Binary application payload bytes accepted by the WebSocket sink; excludes WebSocket and TLS overhead |
| `bytes_received` | Application payload bytes received |
| `tcp_min_rtt_ms` | NDT7 TCPInfo minimum RTT in milliseconds |
| `tcp_rtt_ms` | NDT7 TCPInfo current/smoothed RTT in milliseconds |
| `tcp_retransmissions` | NDT7 TCPInfo retransmitted-byte/count metric supplied by server |
| `os_error_code` | Operating-system error number when available |
| `error_kind` | Stable machine-readable failure classification |
| `error_message` | Sanitized human diagnostic; may change between versions |

## Outcomes

| Outcome | Meaning |
| --- | --- |
| `success` | Requested operation completed |
| `partial` | At least one bandwidth direction completed |
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

During automatic bandwidth tests in `run`, ping rounds continue on the selected bandwidth
interface. A ping is under load when `load_phase` is `download` or `upload`; `setup`
covers discovery and connection work that does not itself represent throughput load.
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
