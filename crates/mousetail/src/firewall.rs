//! Whether this computer's firewall keeps other computers from reaching MouseTail.
//!
//! Some Linux desktops (Omarchy, Fedora) turn a firewall on that drops whatever it hasn't been
//! told to let in. MouseTail still connects when it dials out (the firewall lets the replies
//! back), but nothing can dial in, so linking up depends on which side finds the other first.
//! This only reads the firewall's settings (changing them needs a password: see
//! `enable-firewall.sh`), and says nothing when it can't tell.

// The parsing is tested everywhere but only used on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// The firewall in the way of MouseTail's UDP `port`, if any ("ufw", "firewalld").
#[cfg(target_os = "linux")]
pub fn blocking(port: u16) -> Option<&'static str> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    if let Some(conf) = read("/etc/ufw/ufw.conf")
        && ufw_enabled(&conf)
    {
        let policy = read("/etc/default/ufw").unwrap_or_default();
        // Rules can't be read (some distributions hide them): can't tell, so don't say.
        let rules = read("/etc/ufw/user.rules")?;
        return (!ufw_input_accepts(&policy) && !ufw_rules_allow(&rules, port)).then_some("ufw");
    }
    firewalld_blocks(port).then_some("firewalld")
}

#[cfg(not(target_os = "linux"))]
pub fn blocking(_port: u16) -> Option<&'static str> {
    None
}

/// firewalld is running and its default zone neither trusts everything nor opens the port.
#[cfg(target_os = "linux")]
fn firewalld_blocks(port: u16) -> bool {
    use std::process::{Command, Stdio};
    let ask = |args: &[&str]| -> Option<String> {
        let out = Command::new("firewall-cmd")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    if ask(&["--state"]).as_deref() != Some("running") {
        return false;
    }
    if ask(&["--get-default-zone"]).as_deref() == Some("trusted") {
        return false;
    }
    // "no" only: anything else (not allowed to ask, say) means we can't tell.
    if ask(&["--query-port", &format!("{port}/udp")]).as_deref() != Some("no") {
        return false;
    }
    // enable-firewall.sh opens it to local networks with rich rules, which that doesn't see.
    let rich = ask(&["--list-rich-rules"]).unwrap_or_default();
    !firewalld_rich_rules_allow(&rich, port)
}

/// Some firewalld rich rule accepts UDP `port`.
fn firewalld_rich_rules_allow(rules: &str, port: u16) -> bool {
    let wanted = format!("port port=\"{port}\" protocol=\"udp\"");
    rules
        .lines()
        .any(|r| r.contains(&wanted) && r.trim_end().ends_with("accept"))
}

/// `/etc/ufw/ufw.conf` turns it on.
fn ufw_enabled(conf: &str) -> bool {
    conf.lines()
        .map(str::trim)
        .any(|l| l.eq_ignore_ascii_case("ENABLED=yes"))
}

/// `/etc/default/ufw` lets everything in unless told otherwise.
fn ufw_input_accepts(defaults: &str) -> bool {
    defaults
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("DEFAULT_INPUT_POLICY="))
        .any(|v| v.trim_matches('"') == "ACCEPT")
}

/// Some rule in `/etc/ufw/user.rules` lets UDP `port` in, from anywhere or somewhere (a rule
/// for the local network counts: that's where the other computers are).
fn ufw_rules_allow(rules: &str, port: u16) -> bool {
    rules.lines().any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let arg = |flag: &str| {
            words
                .iter()
                .position(|w| *w == flag)
                .and_then(|i| words.get(i + 1))
                .copied()
        };
        if words.first() != Some(&"-A")
            || words.get(1) != Some(&"ufw-user-input")
            || !matches!(arg("-j"), Some("ACCEPT" | "ufw-user-limit-accept"))
        {
            return false;
        }
        let proto_ok = arg("-p").is_none_or(|p| p == "udp" || p == "all");
        let port_ok = match (arg("--dport"), arg("--dports")) {
            (Some(p), _) => ports_include(p, port),
            (None, Some(list)) => list.split(',').any(|p| ports_include(p, port)),
            (None, None) => true,
        };
        proto_ok && port_ok
    })
}

/// "24802" or a range "24800:24810".
fn ports_include(spec: &str, port: u16) -> bool {
    match spec.split_once(':') {
        Some((lo, hi)) => match (lo.parse::<u16>(), hi.parse::<u16>()) {
            (Ok(lo), Ok(hi)) => (lo..=hi).contains(&port),
            _ => false,
        },
        None => spec.parse() == Ok(port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // As Omarchy sets it up, plus a Lan Mouse rule from one address.
    const OMARCHY: &str = "\
*filter
### RULES ###
### tuple ### allow udp 53317 0.0.0.0/0 any 0.0.0.0/0 in
-A ufw-user-input -p udp --dport 53317 -j ACCEPT
-A ufw-user-input -p tcp --dport 53317 -j ACCEPT
-A ufw-user-input -p udp -d 172.17.0.1 --dport 53 -s 172.16.0.0/12 -j ACCEPT
-A ufw-user-input -p tcp --dport 22 -m conntrack --ctstate NEW -m recent --set
-A ufw-user-input -p tcp --dport 22 -j ufw-user-limit-accept
-A ufw-user-input -p udp --dport 4242 -s 192.168.68.64 -j ACCEPT
### END RULES ###
-A ufw-user-limit -j REJECT
";

    #[test]
    fn omarchy_blocks_mousetail() {
        assert!(ufw_enabled("# comment\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(!ufw_enabled("ENABLED=no\n"));
        assert!(!ufw_input_accepts("DEFAULT_INPUT_POLICY=\"DROP\"\n"));
        assert!(ufw_input_accepts("DEFAULT_INPUT_POLICY=\"ACCEPT\"\n"));
        assert!(!ufw_rules_allow(OMARCHY, 24802));
        assert!(ufw_rules_allow(OMARCHY, 53317));
        assert!(ufw_rules_allow(OMARCHY, 4242));
        assert!(!ufw_rules_allow(OMARCHY, 22)); // tcp only
    }

    #[test]
    fn firewalld_rules_from_enable_firewall() {
        let rules = "rule family=\"ipv4\" source address=\"192.168.0.0/16\" port port=\"24802\" protocol=\"udp\" accept\n";
        assert!(firewalld_rich_rules_allow(rules, 24802));
        assert!(!firewalld_rich_rules_allow(rules, 24803));
        assert!(!firewalld_rich_rules_allow("", 24802));
    }

    #[test]
    fn rules_that_let_mousetail_in() {
        let allows = |rule: &str| ufw_rules_allow(rule, 24802);
        assert!(allows("-A ufw-user-input -p udp --dport 24802 -j ACCEPT"));
        assert!(allows(
            "-A ufw-user-input -p udp --dport 24802 -s 192.168.0.0/16 -j ACCEPT"
        ));
        assert!(allows(
            "-A ufw-user-input -p udp -m multiport --dports 22,24800:24810 -j ACCEPT"
        ));
        assert!(allows("-A ufw-user-input -s 192.168.68.0/22 -j ACCEPT"));
        assert!(!allows("-A ufw-user-input -p tcp --dport 24802 -j ACCEPT"));
        assert!(!allows("-A ufw-user-input -p udp --dport 24802 -j DROP"));
        assert!(!allows("-A ufw-user-output -p udp --dport 24802 -j ACCEPT"));
        assert!(!allows(
            "### tuple ### allow udp 24802 0.0.0.0/0 any 0.0.0.0/0 in"
        ));
    }
}
