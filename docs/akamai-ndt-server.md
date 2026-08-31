# Run Netband against NDT7 on Akamai Cloud

This guide deploys M-Lab's [`ndt-server`](https://github.com/m-lab/ndt-server)
on an Akamai Cloud compute instance (formerly Linode) and configures Netband to
use it directly. It does **not** put the server behind the Akamai CDN. A CDN,
load balancer, or application proxy can become the measured bottleneck and make
the result describe that intermediary instead of the path to the NDT server.

There is no universally optimal region or instance size. The least expensive
accurate deployment is the smallest instance whose CPU and network limits remain
above the connection being measured. Start small, verify it, and resize only
when the server is the bottleneck.

## 1. Choose the region and plan

Use Akamai's [data center speed tests](https://techdocs.akamai.com/cloud-computing/docs/how-to-choose-a-data-center)
from the network where Netband will run. Test the nearest two or three core
regions several times and choose the one with the best combination of latency,
route stability, and download throughput. A geographically closer region is not
always better routed.

For a low-frequency home or lab monitor, begin with the smallest Shared CPU plan
whose documented `Network Out` rate exceeds the fastest connection you need to
measure. Move to Dedicated CPU if:

- results repeatedly plateau below the instance's advertised network rate;
- the container is CPU-bound during a test; or
- repeatability matters more than the additional compute and transfer cost.

Core-region Shared CPU plans generally provide the lowest starting cost and may
include pooled outbound transfer. Newer Dedicated CPU and distributed-region
plans may bill all transfer. Check the current [Akamai Cloud pricing](https://www.akamai.com/cloud/pricing)
and the selected plan's transfer allowance before creating the instance.

NDT7's download direction is outbound traffic from the server, so it consumes
the Akamai transfer allowance. The upload direction is inbound to the server,
which Akamai does not meter. A conservative monthly download estimate is:

```text
GiB/month ~= download Mbps * 10 seconds * runs/day * days / 8,590
```

For example, four full 1 Gbps downloads per day for 30 days are about 140 GiB.
Actual use depends on achieved throughput and interrupted tests. Review Akamai's
[network transfer rules](https://techdocs.akamai.com/cloud-computing/docs/network-transfer-usage-and-costs)
and monitor the account transfer pool after deployment.

## 2. Create and secure the instance

Create one public Akamai Cloud compute instance with:

- the latest Ubuntu LTS image;
- a DNS name such as `ndt.example.com` pointing directly to its public address;
- a Cloud Firewall with a default inbound policy of `Drop`;
- TCP 22 allowed only from an administration address; and
- TCP 443 allowed publicly for the ACME setup below, or only from the Netband
  clients when using a separately managed certificate.

Follow Akamai's [compute instance](https://techdocs.akamai.com/cloud-computing/docs/create-a-compute-instance)
and [Cloud Firewall](https://techdocs.akamai.com/cloud-computing/docs/getting-started-with-cloud-firewalls)
guides. Do not expose the NDT5, cleartext NDT7, health, or Prometheus ports.

The setup below uses `ndt-server`'s ACME support, which requires the DNS record
to exist and public TCP 443 access for certificate issuance and renewal. A
publicly reachable NDT server can be used by anyone and can create unplanned
egress charges. When every Netband site has a stable public address, the safer
option is to allow only those source CIDRs and supply a certificate renewed with
a DNS-01 challenge instead of the ACME flags shown below.

Install Docker Engine using Docker's [Ubuntu instructions](https://docs.docker.com/engine/install/ubuntu/),
then enable BBR as recommended by the upstream NDT server:

```sh
sudo modprobe tcp_bbr
printf '%s\n' \
  'net.core.default_qdisc=fq' \
  'net.ipv4.tcp_congestion_control=bbr' |
  sudo tee /etc/sysctl.d/90-ndt-server.conf >/dev/null
sudo sysctl --system
sysctl net.ipv4.tcp_congestion_control
```

The last command must report `bbr`.

## 3. Build and run `ndt-server`

The upstream project documents building its container locally. Pinning the
release and commit keeps the deployment reproducible; review and deliberately
update both values when adopting a newer release.

```sh
git clone --branch v0.25.3 --depth 1 https://github.com/m-lab/ndt-server.git
cd ndt-server
test "$(git rev-parse HEAD)" = d724ae67fd56f6b089e40b1d9c8ff0f5e6ddc632
sudo docker build --pull --tag ndt-server:v0.25.3 .
```

Create writable storage for measurement results and ACME certificates. Replace
the example hostname with the DNS record created above.

```sh
NDT_HOST=ndt.example.com

sudo install -d -m 0755 /etc/ndt-server
sudo install -d -m 0750 -o 65534 -g 65534 \
  /var/lib/ndt-server/results /var/lib/ndt-server/autocert
printf '%s' "$NDT_HOST" | sudo tee /etc/ndt-server/hostname >/dev/null

sudo docker run --detach \
  --name ndt-server \
  --restart unless-stopped \
  --stop-timeout 30 \
  --publish 443:4443/tcp \
  --volume /etc/ndt-server/hostname:/hostname:ro \
  --volume /var/lib/ndt-server/results:/datadir \
  --volume /var/lib/ndt-server/autocert:/autocert \
  --read-only \
  --user 65534:65534 \
  --cap-drop ALL \
  --security-opt no-new-privileges=true \
  ndt-server:v0.25.3 \
  -autocert.enabled=true \
  -autocert.hostname=@/hostname \
  -autocert.dir=/autocert \
  -datadir=/datadir \
  -ndt7_addr=:4443 \
  -ndt7_addr_cleartext=127.0.0.1:8080 \
  -ndt5_addr=127.0.0.1:3001 \
  -ndt5_ws_addr=127.0.0.1:3002 \
  -ndt5_wss_addr=127.0.0.1:3010 \
  -health_addr=127.0.0.1:8000 \
  -prometheusx.listen-address=127.0.0.1:9990 \
  -tls.version=1.3
```

Confirm that the container remains healthy and that the public certificate is
valid:

```sh
sudo docker ps --filter name=ndt-server
sudo docker logs --tail 50 ndt-server
curl --fail --silent --show-error "https://${NDT_HOST}/ndt7.html" >/dev/null
```

If you use a separately managed certificate, mount its certificate and key
read-only and replace the three `autocert` arguments with `-cert=/certs/cert.pem`
and `-key=/certs/key.pem`.

## 4. Configure Netband

Add the direct provider to the Netband configuration. The daily cap limits cost,
while Netband distributes scheduled runs across the UTC day with jitter and
recalculates remaining opportunities after a ping-triggered run.

```toml
[bandwidth]
provider = "direct"
daily_max = 4
min_spacing = "3h"
slot_jitter_pct = 50

[bandwidth.direct]
target = "ndt.example.com:443"
```

The cap applies to clients sharing this Netband state file, not to the NDT
server. Other clients are not constrained by it, and `once bandwidth --force`
bypasses direct-provider limits. Do not use `--force` for routine monitoring.

No M-Lab consent flag is needed for a direct server. Validate the configuration,
perform one measurement, and then start normal monitoring:

```sh
netband --config /etc/netband/netband.toml config check
netband --config /etc/netband/netband.toml once bandwidth
netband --config /etc/netband/netband.toml run
```

The standard target expands to `wss://ndt.example.com/ndt/v7/download` and
`wss://ndt.example.com/ndt/v7/upload`. See [Configuration and providers](configuration.md#direct-provider)
for private-CA, IP-address, and nonstandard-path variants.

## 5. Verify cost and accuracy

Review the first week before treating the results as a baseline:

1. Compare at least three tests at quiet and busy times of day.
2. Check `docker stats ndt-server` during a test. Sustained CPU saturation means
   the instance may be limiting the result.
3. Compare measured download speed with the plan's `Network Out` limit. A result
   clustered near that limit requires a larger plan for accurate faster links.
4. Inspect Netband's CSV `outcome`, `server`, duration, and error fields for
   timeouts or failed directions.
5. Check Akamai Cloud Manager's monthly transfer pool and instance network graph.

NDT7 measures the end-to-end path to this particular server. It does not prove
the ISP's maximum speed to every destination. Keep the hostname, region, plan,
BBR setting, and daily schedule stable when comparing results over time.

The server retains per-test result files under `/var/lib/ndt-server/results` and
container logs may contain client network details. Set retention and access
controls appropriate for the deployment; see [Netband privacy](../PRIVACY.md)
for the client-side data model.
