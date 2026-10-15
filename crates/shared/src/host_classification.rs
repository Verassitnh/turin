//! Port of upstream/packages/shared/src/hostClassification.ts
//!
//! Not remote-only: favicon disclosure (`isPublicFaviconHost`) and private-network
//! checks are used by kept UI. Tailscale helpers stay because they live in this file.

use turin_contracts::base_schemas::js_trim;

pub fn normalize_hostname(host: &str) -> String {
    let lowered = host.to_lowercase();
    let stripped = strip_one_bracket(&lowered);
    stripped.trim_end_matches('.').to_string()
}

/// `/^\[|\]$/g` removes one leading `[` and one trailing `]`.
fn strip_one_bracket(host: &str) -> String {
    let mut result = host.to_string();
    if let Some(stripped) = result.strip_prefix('[') {
        result = stripped.to_string();
    }
    if let Some(stripped) = result.strip_suffix(']') {
        result = stripped.to_string();
    }
    result
}

/// JS `Number(part)` for the strings IPv4 splitting produces.
fn js_number(raw: &str) -> f64 {
    let trimmed = js_trim(raw);
    if trimmed.is_empty() {
        return 0.0;
    }
    let (negative, body) = if let Some(rest) = trimmed.strip_prefix('+') {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_prefix('-') {
        (true, rest)
    } else {
        (false, trimmed)
    };
    let magnitude = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map(|n| n as f64)
            .unwrap_or(f64::NAN)
    } else {
        body.parse::<f64>().unwrap_or(f64::NAN)
    };
    if negative { -magnitude } else { magnitude }
}

fn is_js_integer_in_byte(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && (0.0..=255.0).contains(&value)
}

fn parse_ipv4_address(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<f64> = normalize_hostname(host).split('.').map(js_number).collect();
    if parts.len() != 4 || !parts.iter().copied().all(is_js_integer_in_byte) {
        return None;
    }
    Some([
        parts[0] as u8,
        parts[1] as u8,
        parts[2] as u8,
        parts[3] as u8,
    ])
}

fn parse_hex_hextet(part: &str) -> Option<u16> {
    if !part.chars().all(|ch| ch.is_ascii_hexdigit()) || part.is_empty() || part.len() > 4 {
        return None;
    }
    u16::from_str_radix(part, 16).ok()
}

fn parse_ipv4_mapped_ipv6_address(host: &str) -> Option<[u8; 4]> {
    let normalized = normalize_hostname(host);
    let suffix = normalized.strip_prefix("::ffff:")?;
    if let Some(dotted) = parse_ipv4_address(suffix) {
        return Some(dotted);
    }
    let hextets: Vec<&str> = suffix.split(':').collect();
    if hextets.len() != 2 {
        return None;
    }
    let high = parse_hex_hextet(hextets[0])? as u32;
    let low = parse_hex_hextet(hextets[1])? as u32;
    Some([
        (high >> 8) as u8,
        (high & 0xff) as u8,
        (low >> 8) as u8,
        (low & 0xff) as u8,
    ])
}

fn parse_ipv6_address(host: &str) -> Option<[u16; 8]> {
    let normalized = normalize_hostname(host);
    if !normalized.contains(':') {
        return None;
    }
    let halves: Vec<&str> = normalized.split("::").collect();
    if halves.len() > 2 {
        return None;
    }
    let head: Vec<&str> = if halves[0].is_empty() {
        Vec::new()
    } else {
        halves[0].split(':').collect()
    };
    let tail: Vec<&str> = if halves.len() == 2 && !halves[1].is_empty() {
        halves[1].split(':').collect()
    } else {
        Vec::new()
    };
    if head
        .iter()
        .chain(tail.iter())
        .any(|part| parse_hex_hextet(part).is_none())
    {
        return None;
    }
    let missing = 8isize - head.len() as isize - tail.len() as isize;
    if (halves.len() == 1 && missing != 0) || (halves.len() == 2 && missing < 1) {
        return None;
    }
    let mut out = [0u16; 8];
    let mut index = 0;
    for part in &head {
        out[index] = parse_hex_hextet(part)?;
        index += 1;
    }
    index += missing as usize;
    for part in &tail {
        out[index] = parse_hex_hextet(part)?;
        index += 1;
    }
    Some(out)
}

fn ipv6_prefix_matches(address: &[u16; 8], prefix: &[u16; 8], prefix_length: u32) -> bool {
    let full_hextets = (prefix_length / 16) as usize;
    if address[..full_hextets]
        .iter()
        .zip(prefix[..full_hextets].iter())
        .any(|(part, expected)| part != expected)
    {
        return false;
    }
    let remaining_bits = prefix_length % 16;
    if remaining_bits == 0 {
        return true;
    }
    // JS `<<` is 32-bit, so the mask is reduced with `& 0xffff` before it is applied.
    let mask = ((0xffffu32 << (16 - remaining_bits)) & 0xffff) as u16;
    (address[full_hextets] & mask) == (prefix[full_hextets] & mask)
}

fn is_private_ipv4_address(parts: [u8; 4]) -> bool {
    parts[0] == 0
        || parts[0] == 10
        || parts[0] == 127
        || (parts[0] == 100 && (64..=127).contains(&parts[1]))
        || (parts[0] == 172 && (16..=31).contains(&parts[1]))
        || (parts[0] == 192 && parts[1] == 168)
        || (parts[0] == 169 && parts[1] == 254)
        || (parts[0] == 198 && (18..=19).contains(&parts[1]))
}

fn is_special_purpose_ipv4_address(parts: [u8; 4]) -> bool {
    is_private_ipv4_address(parts)
        || parts[0] >= 224
        // Deliberately suppress the whole protocol-assignment block. IANA marks
        // .9 and .10 globally reachable, but privacy-safe false negatives are
        // preferable to disclosing another special-purpose address by mistake.
        || (parts[0] == 192 && parts[1] == 0 && parts[2] == 0)
        || (parts[0] == 192 && parts[1] == 0 && parts[2] == 2)
        || (parts[0] == 192 && parts[1] == 88 && parts[2] == 99)
        || (parts[0] == 198 && parts[1] == 51 && parts[2] == 100)
        || (parts[0] == 203 && parts[1] == 0 && parts[2] == 113)
}

pub fn is_local_loopback_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized == "localhost" || normalized == "::1" {
        return true;
    }
    parse_ipv4_address(&normalized).is_some_and(|parts| parts[0] == 127)
}

/// A name or address only Tailscale uses: a MagicDNS name or its IPv6 range
/// fd7a:115c:a1e0::/48. Its IPv4 addresses come from the shared 100.64.0.0/10
/// range, so they prove nothing on their own.
pub fn is_tailnet_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized.ends_with(".ts.net") {
        return true;
    }
    let Some(ipv6) = parse_ipv6_address(&normalized) else {
        return false;
    };
    ipv6_prefix_matches(&ipv6, &[0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0], 48)
}

/// An IPv4 address in 100.64.0.0/10. Tailscale, Cloudflare WARP and Mesh, other
/// VPNs, and carrier-grade NAT all assign from this range.
pub fn is_shared_address_space_host(host: &str) -> bool {
    parse_ipv4_address(host).is_some_and(|parts| parts[0] == 100 && (64..=127).contains(&parts[1]))
}

pub fn is_private_network_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized == "::"
        || is_local_loopback_host(&normalized)
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
        || normalized == "home.arpa"
        || normalized.ends_with(".home.arpa")
        || (!normalized.contains('.') && !normalized.contains(':'))
    {
        return true;
    }
    if normalized.ends_with(".ts.net") {
        return true;
    }
    if let Some(parts) =
        parse_ipv4_address(&normalized).or_else(|| parse_ipv4_mapped_ipv6_address(&normalized))
    {
        return is_private_ipv4_address(parts);
    }
    let first_ipv6_token = normalized.split(':').next().unwrap_or("");
    if !normalized.contains(':') || parse_hex_hextet(first_ipv6_token).is_none() {
        return false;
    }
    let first_ipv6_hextet = u32::from(parse_hex_hextet(first_ipv6_token).unwrap_or(0));
    (first_ipv6_hextet & 0xfe00) == 0xfc00 || (first_ipv6_hextet & 0xffc0) == 0xfe80
}

const PRIVATE_FAVICON_TLDS: &[&str] = &[
    ".alt",
    ".corp",
    ".example",
    ".home",
    ".internal",
    ".intranet",
    ".invalid",
    ".lan",
    ".onion",
    ".private",
    ".test",
];
const INTERNAL_HOST_LABELS: &[&str] = &["corp", "internal", "intranet"];

/// Whether a hostname is eligible to be disclosed to a public favicon provider.
pub fn is_public_favicon_host(host: &str) -> bool {
    // A single trailing dot is a valid absolute DNS name. Repeated trailing
    // dots are malformed and can conceal legacy numeric forms such as 127.1.
    if host.ends_with("..") {
        return false;
    }
    let normalized = normalize_hostname(host);
    if is_private_network_host(&normalized) {
        return false;
    }
    if PRIVATE_FAVICON_TLDS
        .iter()
        .any(|suffix| normalized == suffix[1..] || normalized.ends_with(suffix))
    {
        return false;
    }
    // Company networks often nest internal services under a public domain
    // (`grafana.internal.acme.com`); never send those names to a third party.
    if normalized
        .split('.')
        .any(|label| INTERNAL_HOST_LABELS.contains(&label))
    {
        return false;
    }
    if let Some(ipv4) =
        parse_ipv4_address(&normalized).or_else(|| parse_ipv4_mapped_ipv6_address(&normalized))
    {
        return !is_special_purpose_ipv4_address(ipv4);
    }
    if !normalized.contains(':') {
        return true;
    }
    let Some(ipv6) = parse_ipv6_address(&normalized) else {
        return false;
    };
    if ipv6_prefix_matches(&ipv6, &[0x0064, 0xff9b, 0, 0, 0, 0, 0, 0], 96) {
        let embedded_ipv4 = [
            (ipv6[6] >> 8) as u8,
            (ipv6[6] & 0xff) as u8,
            (ipv6[7] >> 8) as u8,
            (ipv6[7] & 0xff) as u8,
        ];
        return !is_special_purpose_ipv4_address(embedded_ipv4);
    }
    let first = ipv6[0];
    if (u32::from(first) & 0xe000) != 0x2000 {
        return false;
    }
    if ipv6_prefix_matches(&ipv6, &[0x2001, 0, 0, 0, 0, 0, 0, 0], 23) {
        let public_protocol_assignment = (ipv6[1] == 1
            && ipv6[2..7].iter().all(|part| *part == 0)
            && [1, 2, 3].contains(&ipv6[7]))
            || ipv6_prefix_matches(&ipv6, &[0x2001, 3, 0, 0, 0, 0, 0, 0], 32)
            || ipv6_prefix_matches(&ipv6, &[0x2001, 4, 0x0112, 0, 0, 0, 0, 0], 48)
            || ipv6_prefix_matches(&ipv6, &[0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28)
            || ipv6_prefix_matches(&ipv6, &[0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28);
        return public_protocol_assignment;
    }
    if ipv6_prefix_matches(&ipv6, &[0x2001, 0x0db8, 0, 0, 0, 0, 0, 0], 32) {
        return false;
    }
    if ipv6_prefix_matches(&ipv6, &[0x2002, 0, 0, 0, 0, 0, 0, 0], 16) {
        return false;
    }
    if first == 0x3fff && (ipv6[1] & 0xf000) == 0 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_public_hosts_as_public() {
        for host in [
            "github.com",
            "www.google.com",
            "t3.chat",
            "sub.domain.example.co.uk",
            "8.8.8.8",
            "1.1.1.1",
            "100.200.1.1",
            "172.32.0.1",
            "192.167.1.1",
            "11.0.0.1",
        ] {
            assert!(is_public_favicon_host(host), "{host}");
        }
    }

    #[test]
    fn detects_private_ipv4_ranges() {
        for host in [
            "0.0.0.0",
            "10.0.0.1",
            "10.255.255.255",
            "127.0.0.1",
            "192.168.1.10",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.1.1",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
    }

    #[test]
    fn detects_the_tailscale_shared_range() {
        for host in [
            "100.64.0.1",
            "100.100.100.100",
            "100.126.17.15",
            "100.127.255.255",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
        assert!(is_public_favicon_host("100.63.255.255"));
        assert!(is_public_favicon_host("100.128.0.1"));
    }

    #[test]
    fn detects_private_host_names_and_suffixes() {
        for host in [
            "localhost",
            "air",
            "printer.local",
            "api.internal",
            "router.home.arpa",
            "home.arpa",
            "box.tailnet.ts.net",
            "AIR.TAILE8BEA7.TS.NET",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
    }

    #[test]
    fn detects_private_ipv6_addresses() {
        for host in [
            "::1",
            "[::1]",
            "fd00::1",
            "fc00::1",
            "fe80::1",
            "FD12:3456::1",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
        assert!(is_public_favicon_host("2606:4700:4700::1111"));
    }

    #[test]
    fn detects_ipv4_mapped_ipv6_addresses() {
        for host in [
            "::ffff:192.168.1.10",
            "::ffff:10.0.0.1",
            "::ffff:100.126.17.15",
            "[::ffff:192.168.1.10]",
            "::ffff:c0a8:010a",
            "::ffff:a00:1",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
        assert!(is_public_favicon_host("::ffff:8.8.8.8"));
        assert!(is_public_favicon_host("::ffff:808:808"));
    }

    #[test]
    fn ignores_a_trailing_dns_root_label() {
        for host in [
            "localhost.",
            "printer.local.",
            "api.internal.",
            "box.tailnet.ts.net.",
            "air.",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
        assert!(is_public_favicon_host("github.com."));
    }

    #[test]
    fn detects_names_under_localhost() {
        for host in ["app.localhost", "api.app.localhost", "APP.LOCALHOST"] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
    }

    #[test]
    fn treats_an_empty_host_as_private() {
        assert!(!is_public_favicon_host(""));
        assert!(!is_public_favicon_host("   "));
    }

    #[test]
    fn rejects_malformed_ipv4_text_as_a_public_host() {
        assert!(is_public_favicon_host("10.0.0.999"));
        assert!(is_public_favicon_host("10.0.0"));
    }
}
