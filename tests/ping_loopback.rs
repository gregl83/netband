//! Optional real-socket coverage. Uses only loopback; no network configuration changes.
#![cfg(target_os = "linux")]

use std::net::IpAddr;
use std::time::Duration;

use netband::ping::{PingTransport, ProbeRequest, SurgePingTransport};

#[tokio::test]
#[ignore = "requires Linux IPv4/IPv6 loopback and permission to create ICMP sockets"]
async fn real_echo_replies_preserve_binding_family_and_sequence() {
    for target in ["127.0.0.1", "::1"].map(|value| value.parse::<IpAddr>().unwrap()) {
        let transport = SurgePingTransport::new(None, &[target, target]);
        for sequence in [0, u16::MAX] {
            let attempt = transport
                .probe(ProbeRequest {
                    target,
                    identifier: 321,
                    sequence,
                    timeout: Duration::from_secs(2),
                })
                .await;
            let reply = attempt.result.expect("local echo reply");
            assert!(attempt.sent);
            assert_eq!(attempt.binding.interface, None);
            assert_eq!(attempt.binding.source_ip, Some(target));
            assert_eq!(reply.target, target);
            assert!(reply.identifier.is_none_or(|identifier| identifier == 321));
            assert_eq!(reply.sequence, sequence);
            assert_eq!(reply.icmp_type, if target.is_ipv4() { 0 } else { 129 });
            assert_eq!(reply.icmp_code, 0);
            assert!(reply.rtt <= Duration::from_secs(2));
        }
    }
}
