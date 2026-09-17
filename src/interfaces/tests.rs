use super::*;
use if_addrs::{IfAddr, IfOperStatus, Ifv4Addr, Ifv6Addr, Interface};

fn interface(name: &str, address: &str, status: IfOperStatus) -> Interface {
    let addr = match address.parse::<IpAddr>().unwrap() {
        IpAddr::V4(ip) => IfAddr::V4(Ifv4Addr {
            ip,
            netmask: "255.255.255.0".parse().unwrap(),
            prefixlen: 24,
            broadcast: None,
        }),
        IpAddr::V6(ip) => IfAddr::V6(Ifv6Addr {
            ip,
            netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
            prefixlen: 64,
            broadcast: None,
        }),
    };
    Interface {
        name: name.to_owned(),
        addr,
        index: None,
        oper_status: status,
        is_p2p: false,
        #[cfg(windows)]
        adapter_name: name.to_owned(),
    }
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn missing_interfaces_are_not_confused_with_similar_names() {
    for snapshot in [
        vec![],
        vec![interface("eth01", "192.0.2.1", IfOperStatus::Up)],
        vec![interface("ETH0", "192.0.2.1", IfOperStatus::Up)],
    ] {
        assert_eq!(
            resolve_from("eth0", snapshot),
            Err(InterfaceError::Missing {
                name: "eth0".into()
            })
        );
    }
}

#[test]
fn every_non_operational_status_is_down_even_with_an_address() {
    for status in [
        IfOperStatus::Down,
        IfOperStatus::Testing,
        IfOperStatus::Unknown,
        IfOperStatus::Dormant,
        IfOperStatus::NotPresent,
        IfOperStatus::LowerLayerDown,
    ] {
        assert_eq!(
            resolve_from(
                "eth0",
                vec![
                    interface("eth0", "192.0.2.1", status),
                    interface("other", "192.0.2.2", IfOperStatus::Up),
                ]
            ),
            Err(InterfaceError::Down {
                name: "eth0".into()
            })
        );
    }
}

#[test]
fn unspecified_and_down_addresses_cannot_supply_an_up_interface() {
    assert_eq!(
        resolve_from(
            "eth0",
            vec![
                interface("eth0", "0.0.0.0", IfOperStatus::Up),
                interface("eth0", "::", IfOperStatus::Up),
                interface("eth0", "192.0.2.1", IfOperStatus::Down),
                interface("eth0", "2001:db8::1", IfOperStatus::Down),
            ]
        ),
        Err(InterfaceError::Addressless {
            name: "eth0".into()
        })
    );
}

#[test]
fn dual_stack_addresses_are_filtered_sorted_and_deduplicated_independent_of_discovery_order() {
    let snapshot = vec![
        interface("eth0", "2001:db8::2", IfOperStatus::Up),
        interface("eth0", "192.0.2.2", IfOperStatus::Up),
        interface("eth0", "192.0.2.1", IfOperStatus::Up),
        interface("eth0", "2001:db8::1", IfOperStatus::Up),
        interface("eth0", "192.0.2.2", IfOperStatus::Up),
        interface("eth0", "2001:db8::1", IfOperStatus::Up),
        interface("eth0", "0.0.0.0", IfOperStatus::Up),
        interface("eth0", "::", IfOperStatus::Up),
        interface("eth0", "192.0.2.3", IfOperStatus::Down),
        interface("other", "192.0.2.4", IfOperStatus::Up),
    ];
    let expected = ResolvedInterface {
        name: "eth0".into(),
        addresses: ["192.0.2.1", "192.0.2.2", "2001:db8::1", "2001:db8::2"]
            .map(|ip| ip.parse().unwrap())
            .to_vec(),
    };
    for shift in 0..snapshot.len() {
        let mut reordered = snapshot.clone();
        reordered.rotate_left(shift);
        assert_eq!(resolve_from("eth0", reordered), Ok(expected.clone()));
    }
}

#[test]
fn loopback_and_link_local_addresses_remain_available_for_bound_probes() {
    for address in ["127.0.0.1", "::1", "169.254.1.2", "fe80::1"] {
        let resolved =
            resolve_from("local", vec![interface("local", address, IfOperStatus::Up)]).unwrap();
        assert_eq!(resolved.addresses, [address.parse::<IpAddr>().unwrap()]);
    }
}

#[test]
fn default_route_does_not_require_interface_discovery() {
    assert_eq!(resolve_configured(&[]), Ok(vec![]));
    assert_eq!(
        resolve_configured_with(&[], || panic!("default route must not inspect interfaces")),
        Ok(vec![])
    );
}

#[test]
fn configured_order_uses_one_snapshot_and_fails_without_partial_results() {
    let snapshot = || {
        Ok(vec![
            interface("a", "192.0.2.1", IfOperStatus::Up),
            interface("b", "192.0.2.2", IfOperStatus::Up),
        ])
    };
    let resolved = resolve_configured_with(&names(&["b", "a"]), snapshot).unwrap();
    assert_eq!(
        resolved
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(
        resolved[0].addresses,
        ["192.0.2.2".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(
        resolved[1].addresses,
        ["192.0.2.1".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(
        resolve_configured_with(&names(&["a", "missing", "b"]), snapshot),
        Err(InterfaceError::Missing {
            name: "missing".into()
        })
    );
}

#[test]
fn discovery_errors_preserve_the_cause_instead_of_claiming_an_interface_is_missing() {
    let error = resolve_configured_with(&names(&["eth0"]), || {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "interface enumeration denied",
        ))
    })
    .unwrap_err();
    assert_eq!(
        error,
        InterfaceError::Inspect {
            message: "interface enumeration denied".into()
        }
    );
    assert_eq!(
        error.to_string(),
        "cannot inspect network interfaces: interface enumeration denied"
    );
}

#[test]
fn system_resolver_reports_a_nonexistent_interface() {
    // This name exceeds OS interface-name limits and needs no particular NIC or privileges.
    let name = "netband-test-nonexistent-interface";
    let expected = Err(InterfaceError::Missing { name: name.into() });
    assert_eq!(SystemInterfaceResolver.resolve(name), expected);
    assert_eq!(
        resolve_configured(&names(&[name])),
        expected.map(|value| vec![value])
    );
}

#[test]
fn selector_handles_empty_and_unknown_eligibility_without_consuming_a_turn() {
    let eligible = names(&["unknown"]).into_iter().collect();
    assert_eq!(FairInterfaceSelector::new(&[]).select(&eligible), None);
    let mut selector = FairInterfaceSelector::new(&names(&["b", "a"]));
    assert_eq!(selector.select(&HashSet::new()), None);
    assert_eq!(selector.select(&eligible), None);
    selector.record_attempt("unknown");
    assert_eq!(selector.attempts("unknown"), 0);
    let eligible = names(&["a", "b", "unknown"]).into_iter().collect();
    for _ in 0..3 {
        assert_eq!(selector.select(&eligible), Some("b"));
        assert_eq!(selector.attempts("b"), 0);
    }
}

#[test]
fn equal_attempts_prefer_the_least_recent_turn_over_configuration_order() {
    let mut selector = FairInterfaceSelector::new(&names(&["a", "b"]));
    let eligible = names(&["a", "b"]).into_iter().collect();
    selector.record_attempt("b");
    selector.record_attempt("a");
    assert_eq!(selector.select(&eligible), Some("b"));
    selector.record_attempt("b");
    assert_eq!(selector.select(&eligible), Some("a"));
}

#[test]
fn recovered_interfaces_catch_up_without_selecting_ineligible_ones() {
    let mut selector = FairInterfaceSelector::new(&names(&["a", "b"]));
    let only_a = names(&["a"]).into_iter().collect();
    for _ in 0..3 {
        assert_eq!(selector.select(&only_a), Some("a"));
        selector.record_attempt("a");
    }
    let both = names(&["a", "b"]).into_iter().collect();
    for _ in 0..3 {
        assert_eq!(selector.select(&both), Some("b"));
        selector.record_attempt("b");
    }
    assert_eq!(selector.attempts("a"), selector.attempts("b"));
    assert_eq!(selector.select(&both), Some("a"));
}
