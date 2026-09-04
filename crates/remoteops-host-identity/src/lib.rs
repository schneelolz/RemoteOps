//! Conservative local host identity collection for Agent and Controller clients.

use network_interface::{NetworkInterface, NetworkInterfaceConfig};

/// Non-secret local host identity used for display and operational correlation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostIdentity {
    /// System-reported computer name.
    pub hostname: String,
    /// Stable-looking primary physical MAC, if one can be selected confidently.
    pub mac_address: Option<String>,
}

/// Collect the local computer name and a conservative primary MAC address.
#[must_use]
pub fn collect() -> HostIdentity {
    HostIdentity {
        hostname: local_hostname(),
        mac_address: primary_mac_address(),
    }
}

fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|value| value.trim().trim_end_matches('.').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-host".to_owned())
}

fn primary_mac_address() -> Option<String> {
    let interfaces = NetworkInterface::show().ok()?;
    select_primary_mac(interfaces)
}

fn select_primary_mac(interfaces: Vec<NetworkInterface>) -> Option<String> {
    let mut candidates = interfaces
        .into_iter()
        .filter(|interface| !interface.internal && !interface.addr.is_empty())
        .filter(|interface| looks_like_physical_network(&interface.name))
        .filter_map(|interface| {
            let mac = interface.mac_addr.as_deref()?.trim();
            let normalized = normalize_mac(mac)?;
            is_globally_administered(&normalized).then_some((interface.index, normalized))
        })
        .collect::<Vec<_>>();

    candidates.sort_by_key(|(index, _)| *index);
    candidates.dedup_by(|left, right| left.1 == right.1);
    (candidates.len() == 1).then(|| candidates.remove(0).1)
}

fn normalize_mac(value: &str) -> Option<String> {
    let hex = value
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .collect::<Vec<_>>();
    if hex.len() != 12 {
        return None;
    }
    let text = String::from_utf8(hex).ok()?.to_ascii_uppercase();
    Some(
        text.as_bytes()
            .chunks(2)
            .map(std::str::from_utf8)
            .collect::<Result<Vec<_>, _>>()
            .ok()?
            .join(":"),
    )
}

fn is_globally_administered(mac: &str) -> bool {
    let Some(first) = u8::from_str_radix(&mac[0..2], 16).ok() else {
        return false;
    };
    first & 0x01 == 0 && first & 0x02 == 0
}

fn looks_like_physical_network(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    if [
        "loopback",
        "virtual",
        "vmware",
        "vbox",
        "hyper-v",
        "hyperv",
        "veth",
        "docker",
        "container",
        "bridge",
        "tap",
        "tun",
        "utun",
        "tailscale",
        "zerotier",
        "wireguard",
        "vpn",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        return false;
    }

    normalized.contains("ethernet")
        || normalized.contains("wi-fi")
        || normalized.contains("wifi")
        || normalized.contains("wireless")
        || normalized.starts_with("en")
        || normalized.starts_with("eth")
        || normalized.starts_with("wl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_mac_to_uppercase_colon_format() {
        assert_eq!(
            normalize_mac("aa-bb-cc-dd-ee-ff"),
            Some("AA:BB:CC:DD:EE:FF".to_owned())
        );
        assert_eq!(
            normalize_mac("aabb.ccdd.eeff"),
            Some("AA:BB:CC:DD:EE:FF".to_owned())
        );
    }

    #[test]
    fn rejects_invalid_and_non_global_mac_addresses() {
        assert_eq!(normalize_mac("00:11:22:33:44"), None);
        assert!(!is_globally_administered("02:11:22:33:44:55"));
        assert!(!is_globally_administered("01:11:22:33:44:55"));
        assert!(is_globally_administered("00:11:22:33:44:55"));
    }

    #[test]
    fn rejects_virtual_or_tunnel_names() {
        assert!(!looks_like_physical_network("vEthernet (Default Switch)"));
        assert!(!looks_like_physical_network("utun4"));
        assert!(looks_like_physical_network("en0"));
        assert!(looks_like_physical_network("Wi-Fi"));
    }

    #[test]
    fn only_selects_one_active_physical_interface() {
        let active = NetworkInterface::new_afinet(
            "en0",
            std::net::Ipv4Addr::new(192, 0, 2, 10),
            None,
            None,
            1,
            false,
        )
        .with_mac_addr(Some("00:11:22:33:44:55".to_owned()));
        let inactive = NetworkInterface {
            name: "Ethernet 2".to_owned(),
            addr: Vec::new(),
            mac_addr: Some("00:11:22:33:44:66".to_owned()),
            index: 2,
            internal: false,
        };
        let virtual_interface = NetworkInterface::new_afinet(
            "vEthernet (WSL)",
            std::net::Ipv4Addr::new(192, 0, 2, 11),
            None,
            None,
            3,
            false,
        )
        .with_mac_addr(Some("00:11:22:33:44:77".to_owned()));

        assert_eq!(
            select_primary_mac(vec![active, inactive, virtual_interface]),
            Some("00:11:22:33:44:55".to_owned())
        );
    }
}
