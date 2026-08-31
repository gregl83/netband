# Netband privacy notice

Effective: 2026-08-30

Netband is self-hosted command-line software. It does not have a Netband-operated
account, analytics service, telemetry collector, or cloud backend. It actively sends
the network probes you configure and stores results on the machine where it runs.

## Data stored locally

Netband writes an authoritative CSV journal containing timestamps, configured ping
targets, selected interface/source addresses, latency and loss, NDT7 server/remote
addresses, throughput/TCP metrics, request status, and failure details. It also stores
scheduler state, backups, a reservation ledger, and a lock file so provider limits and
cooldowns survive restart. Operational stderr may contain the same categories of
diagnostic data and is retained according to your terminal, redirection, or journald
configuration.

You control the filesystem location, access, backup, sharing, retention, and deletion
of these local files. The example systemd unit limits state files to its dynamic user.
Human and JSONL stdout are optional views; redirecting them creates additional copies
that you must manage.

Endpoint query values and recognized token/key values are redacted from CSV, stdout,
provider fingerprints, and operational diagnostics. Do not treat redaction as a secret
store: protect configurations, private CA files, shell history, process arguments, and
any redirected output using normal operating-system controls.

## Ping traffic

ICMP probes disclose your source IP and timing/traffic metadata to the configured
target and intervening networks. Defaults target public Cloudflare, Google, and Quad9
IP addresses. Configure different targets if those services or their policies are not
appropriate for your environment.

## M-Lab NDT7

M-Lab traffic is disabled for unattended operation until you explicitly set
`accept_mlab_policy = true` or pass `--accept-mlab-policy`. Before doing that, read the
current [M-Lab Acceptable Use Policy](https://www.measurementlab.net/aup/) and
[M-Lab Privacy Policy](https://www.measurementlab.net/privacy/). Netband's notice does
not replace or modify those policies.

As of the linked M-Lab privacy policy version 5 dated May 3, 2026, M-Lab states that
NDT test data can include connection performance, client public IP address, test date
and time, and machine/client metadata; experiment data is retained indefinitely and
published publicly. Public data may be used by third parties. M-Lab also states that
the collection server may be outside the user's country. Review the live policy before
every deployment because M-Lab may update it.

M-Lab's acceptable-use policy requires consent for automated testing, limits automated
clients to no more than four tests per day, and calls for randomized test times. Netband
enforces a maximum of four starts per UTC day for the M-Lab provider, persists manual
and automatic reservations together, spaces them by at least 36 minutes, and randomizes
planned times. A test that starts may transfer substantial data in both directions.

Do not enable M-Lab if the network owner or affected user has not consented, if public
and indefinite test data is unacceptable, or if applicable policy/law prohibits it.
M-Lab states that its services are not intended for anyone under 16.

Questions or rights requests about M-Lab-held data must be directed to M-Lab using the
contact process in its privacy policy. Netband's maintainers do not receive or control
that provider-held data.

## Direct NDT7 providers

Direct mode sends the same general connection/performance traffic to an endpoint chosen
by the operator, but M-Lab's privacy policy and four-run constraint do not automatically
apply. The direct endpoint/CDN operator determines what it collects, publishes, retains,
and shares. Before use, obtain authorization and document that operator's privacy,
retention, capacity, security, and rate policy. Configure an appropriate daily cap and
spacing; adaptive cooldown remains active.

Verified TLS is the default. A private CA can be added without disabling certificate
validation. `--allow-insecure-ndt` permits unencrypted `ws://` only after an explicit
opt-in and should be limited to a trusted private network. Netband does not provide a
public Akamai endpoint or authorization to use any CDN.

## Operator responsibilities

- Inform affected users/network owners about active ICMP and bandwidth probes.
- Review provider policies and obtain required consent before enabling bandwidth.
- Restrict local CSV, state, config, logs, and backups to authorized users.
- Set retention/deletion rules suitable for IP addresses and network diagnostics.
- Account for bandwidth usage, metered connections, and provider capacity.
- Revisit this notice and linked provider policies when upgrading or changing provider.
