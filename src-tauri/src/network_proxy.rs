use crate::environment::EnvironmentMap;

#[cfg(any(windows, test))]
use crate::environment::get_case_insensitive;

#[cfg(any(windows, test))]
struct ManualProxy {
    server: String,
    bypass: Option<String>,
}

/// Additional environment for download tools that only recognize proxy variables.
/// Process-only variables remain inherited; none of the process or registry values
/// are modified here.
pub(crate) fn child_download_environment(overrides: EnvironmentMap) -> EnvironmentMap {
    #[cfg(windows)]
    {
        let inherited = std::env::vars_os()
            .filter_map(|(name, value)| {
                name.into_string()
                    .ok()
                    .map(|name| (name, value.to_string_lossy().into_owned()))
            })
            .collect();
        supplement_manual_proxy(overrides, &inherited, read_manual_proxy().as_ref())
    }
    #[cfg(not(windows))]
    {
        overrides
    }
}

#[cfg(windows)]
fn read_manual_proxy() -> Option<ManualProxy> {
    use winreg::{RegKey, enums::{HKEY_CURRENT_USER, KEY_READ}};

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    if key.get_value::<u32, _>("ProxyEnable").ok()? == 0 {
        return None;
    }
    // PAC scripts and automatic discovery are intentionally not interpreted.
    let server = key.get_value::<String, _>("ProxyServer").ok()?;
    Some(ManualProxy {
        server,
        bypass: key.get_value::<String, _>("ProxyOverride").ok(),
    })
}

#[cfg(any(windows, test))]
fn supplement_manual_proxy(
    mut overrides: EnvironmentMap,
    inherited: &EnvironmentMap,
    manual: Option<&ManualProxy>,
) -> EnvironmentMap {
    let present = |name: &str| {
        get_case_insensitive(&overrides, name).is_some()
            || get_case_insensitive(inherited, name).is_some()
    };
    if present("ALL_PROXY") {
        return overrides;
    }
    let explicit_http = present("HTTP_PROXY");
    let explicit_https = present("HTTPS_PROXY");
    let explicit_no_proxy = present("NO_PROXY");
    let Some(manual) = manual else { return overrides; };
    let (http, https) = parse_proxy_server(&manual.server);
    let mut supplemented = false;
    if !explicit_http && let Some(proxy) = http {
        overrides.insert("HTTP_PROXY".to_string(), proxy);
        supplemented = true;
    }
    if !explicit_https && let Some(proxy) = https {
        overrides.insert("HTTPS_PROXY".to_string(), proxy);
        supplemented = true;
    }
    // NO_PROXY applies to both protocols. Do not apply Windows bypass rules to
    // a protocol whose proxy was explicitly selected by the caller.
    if supplemented && !explicit_http && !explicit_https && !explicit_no_proxy
        && let Some(bypass) = manual.bypass.as_deref().and_then(safe_no_proxy)
    {
        overrides.insert("NO_PROXY".to_string(), bypass);
    }
    overrides
}

#[cfg(any(windows, test))]
fn parse_proxy_server(value: &str) -> (Option<String>, Option<String>) {
    let value = value.trim();
    let assignments = value.split(';').any(|entry| {
        entry.split_once('=').is_some_and(|(name, _)| {
            matches!(name.trim().to_ascii_lowercase().as_str(), "http" | "https" | "ftp" | "socks")
        })
    });
    if !assignments {
        let proxy = normalize_proxy_url(value);
        return (proxy.clone(), proxy);
    }
    let mut values = std::collections::HashMap::new();
    for entry in value.split(';') {
        let Some((name, address)) = entry.split_once('=') else { continue; };
        let name = name.trim().to_ascii_lowercase();
        if !matches!(name.as_str(), "http" | "https") {
            continue;
        }
        let address = normalize_proxy_url(address);
        values.entry(name).and_modify(|existing| {
            if *existing != address {
                *existing = None;
            }
        }).or_insert(address);
    }
    (values.remove("http").flatten(), values.remove("https").flatten())
}

#[cfg(any(windows, test))]
fn normalize_proxy_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) || value.contains(';') {
        return None;
    }
    let value = if value.contains("://") { value.to_string() } else { format!("http://{value}") };
    let proxy = url::Url::parse(&value).ok()?;
    if !matches!(proxy.scheme(), "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h")
        || proxy.host_str().is_none()
        || proxy.port_or_known_default().is_none_or(|port| port == 0)
        || proxy.query().is_some()
        || proxy.fragment().is_some()
        || !matches!(proxy.path(), "" | "/")
    {
        return None;
    }
    Some(proxy.to_string())
}

#[cfg(any(windows, test))]
fn safe_no_proxy(value: &str) -> Option<String> {
    use std::{collections::BTreeSet, net::IpAddr};

    let entries = value.split(';')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect::<BTreeSet<_>>();
    if entries.contains("*") {
        return Some("*".to_string());
    }
    let mut bypass = BTreeSet::new();
    for entry in &entries {
        let address = entry.strip_prefix('[').and_then(|address| address.strip_suffix(']')).unwrap_or(entry);
        if let Ok(ip) = address.parse::<IpAddr>() {
            match ip {
                IpAddr::V4(_) => { bypass.insert(ip.to_string()); }
                IpAddr::V6(_) => {
                    // Rustup's env_proxy compares Url::host_str(), including brackets.
                    // Also retain curl's unbracketed representation for older backends.
                    bypass.insert(ip.to_string());
                    bypass.insert(format!("[{ip}]"));
                }
            }
            continue;
        }
        let Some(domain) = entry.strip_prefix("*.") else { continue; };
        if domain.is_empty()
            || !domain.split('.').all(|label| {
                !label.is_empty()
                    && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                    && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            })
            || !(entries.contains(domain) || (!domain.contains('.') && entries.contains("<local>")))
        {
            continue;
        }
        // NO_PROXY domains include the apex and every subdomain. Translate only
        // when both are covered by Windows rules; otherwise retain proxying.
        bypass.insert(domain.to_string());
    }
    (!bypass.is_empty()).then(|| bypass.into_iter().collect::<Vec<_>>().join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(server: &str, bypass: Option<&str>) -> ManualProxy {
        ManualProxy { server: server.to_string(), bypass: bypass.map(str::to_string) }
    }

    #[test]
    fn windows_shared_and_protocol_specific_proxy_addresses_are_normalized() {
        assert_eq!(parse_proxy_server("127.0.0.1:7890"), (Some("http://127.0.0.1:7890/".into()), Some("http://127.0.0.1:7890/".into())));
        assert_eq!(parse_proxy_server(" HTTP=127.0.0.1:7890;HTTPS=127.0.0.1:7891;socks=127.0.0.1:1080 "), (Some("http://127.0.0.1:7890/".into()), Some("http://127.0.0.1:7891/".into())));
        assert_eq!(parse_proxy_server("http=http://127.0.0.1:7890;https=https://proxy.example:443"), (Some("http://127.0.0.1:7890/".into()), Some("https://proxy.example/".into())));
        assert_eq!(parse_proxy_server("http=127.0.0.1:7890;http=127.0.0.1:7891"), (None, None));
        assert_eq!(parse_proxy_server("socks=127.0.0.1:1080"), (None, None));
        assert_eq!(parse_proxy_server("https://proxy.example/proxy.pac"), (None, None));
    }

    #[test]
    fn explicit_process_and_fresh_registry_proxy_values_including_empty_are_preserved() {
        let inherited = EnvironmentMap::from([("https_proxy".into(), "".into()), ("No_Proxy".into(), "internal.example".into())]);
        let overrides = EnvironmentMap::from([("PATH".into(), r"E:\Tools".into())]);
        let result = supplement_manual_proxy(overrides, &inherited, Some(&manual("127.0.0.1:7890", Some("*"))));
        assert_eq!(result.get("PATH").map(String::as_str), Some(r"E:\Tools"));
        assert!(result.contains_key("HTTP_PROXY"));
        assert!(!result.contains_key("HTTPS_PROXY"));
        assert!(!result.contains_key("NO_PROXY"));
        let overrides = EnvironmentMap::from([("Https_Proxy".into(), "http://explicit:8080".into()), ("no_proxy".into(), "".into())]);
        let result = supplement_manual_proxy(overrides.clone(), &EnvironmentMap::new(), Some(&manual("127.0.0.1:7890", Some("*"))));
        assert_eq!(result.get("Https_Proxy"), overrides.get("Https_Proxy"));
        assert_eq!(result.get("no_proxy"), overrides.get("no_proxy"));
        assert!(!result.contains_key("HTTPS_PROXY"));
        let overrides = EnvironmentMap::from([("no_proxy".into(), "".into())]);
        let result = supplement_manual_proxy(overrides, &EnvironmentMap::new(), Some(&manual("127.0.0.1:7890", Some("*"))));
        assert!(result.contains_key("HTTP_PROXY") && result.contains_key("HTTPS_PROXY"));
        assert_eq!(result.get("no_proxy").map(String::as_str), Some(""));
        assert!(!result.contains_key("NO_PROXY"));
    }

    #[test]
    fn all_proxy_prevents_system_proxy_from_replacing_explicit_routing() {
        for value in ["", "socks5://127.0.0.1:1080"] {
            let inherited = EnvironmentMap::from([("all_proxy".into(), value.into())]);
            let overrides = EnvironmentMap::from([("JAVA_HOME".into(), r"E:\Java".into())]);
            let result = supplement_manual_proxy(overrides.clone(), &inherited, Some(&manual("127.0.0.1:7890", Some("*"))));
            assert_eq!(result, overrides);
        }
        let overrides = EnvironmentMap::from([("ALL_PROXY".into(), "".into())]);
        assert_eq!(supplement_manual_proxy(overrides.clone(), &EnvironmentMap::new(), Some(&manual("127.0.0.1:7890", None))), overrides);
    }

    #[test]
    fn safe_bypass_translation_does_not_expand_windows_domain_or_local_rules() {
        assert_eq!(safe_no_proxy("localhost;*.localhost;example.com;*.example.com;127.0.0.1;[::1]"), Some("127.0.0.1,::1,[::1],example.com,localhost".into()));
        assert_eq!(safe_no_proxy("<local>;*.localhost"), Some("localhost".into()));
        assert_eq!(safe_no_proxy("<local>;localhost;*.example.com;exact.example;127.*;10.0.0.0/8"), None);
        assert_eq!(safe_no_proxy("<local>;*"), Some("*".into()));
    }

    #[test]
    fn absent_manual_proxy_changes_nothing_and_valid_manual_rules_only_extend_child_overrides() {
        let overrides = EnvironmentMap::from([("PATH".into(), r"E:\Tools".into())]);
        assert_eq!(supplement_manual_proxy(overrides.clone(), &EnvironmentMap::new(), None), overrides);
        let settings = manual("127.0.0.1:7890", Some("127.0.0.1;localhost;*.localhost"));
        let result = supplement_manual_proxy(overrides.clone(), &EnvironmentMap::new(), Some(&settings));
        assert_eq!(result.get("HTTPS_PROXY").map(String::as_str), Some("http://127.0.0.1:7890/"));
        assert_eq!(result.get("NO_PROXY").map(String::as_str), Some("127.0.0.1,localhost"));
        assert_eq!(overrides.len(), 1);
    }
}
