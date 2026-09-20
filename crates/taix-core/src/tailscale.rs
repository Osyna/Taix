use std::net::Ipv4Addr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tailnet {
    pub state: String,
    pub name: String,
    pub ip: Option<Ipv4Addr>,
}

/// 5 s cache for `tailscale serve status`. Spawn frequency warrants it.
static SERVE_CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
const SERVE_TTL: Duration = Duration::from_secs(5);

/// 30 s cache: `tailscale status` spawns a process and queries the daemon.
static CACHE: Mutex<Option<(Instant, Option<Tailnet>)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(30);

pub fn invalidate() {
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *SERVE_CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn status() -> Option<Tailnet> {
    let mut cached = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, ref result)) = *cached
        && at.elapsed() < CACHE_TTL
    {
        return result.clone();
    }
    let result = read_status();
    *cached = Some((Instant::now(), result.clone()));
    result
}

fn read_status() -> Option<Tailnet> {
    crate::which("tailscale")?;
    let output = std::process::Command::new("tailscale")
        .args(["status", "--json", "--peers=false"])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            return Some(Tailnet {
                state: "Stopped".to_string(),
                ..Default::default()
            });
        }
    };
    parse(std::str::from_utf8(&output.stdout).unwrap_or(""))
}

fn parse(json: &str) -> Option<Tailnet> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let state = v["BackendState"].as_str().unwrap_or("").to_string();
    let name = v["Self"]["DNSName"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    let ip = v["Self"]["TailscaleIPs"]
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str())
        .find_map(|s| s.parse::<Ipv4Addr>().ok());
    Some(Tailnet { state, name, ip })
}

/// `tailscale serve` needs the tailnet to have HTTPS certs enabled. The
/// error string is what the user is shown.
pub fn serve_url() -> Option<String> {
    let mut cached = SERVE_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, ref result)) = *cached
        && at.elapsed() < SERVE_TTL
    {
        return result.clone();
    }
    let result = read_serve_url();
    *cached = Some((Instant::now(), result.clone()));
    result
}

pub fn serve_on(port: u16) -> Result<(), String> {
    crate::which("tailscale").ok_or("tailscale not found")?;
    let output = std::process::Command::new("tailscale")
        .args([
            "serve",
            "--bg",
            "--https=443",
            &format!("http://127.0.0.1:{port}"),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    invalidate();
    Ok(())
}

pub fn serve_off() -> Result<(), String> {
    crate::which("tailscale").ok_or("tailscale not found")?;
    let mut output = std::process::Command::new("tailscale")
        .args(["serve", "--https=443", "off"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        output = std::process::Command::new("tailscale")
            .args(["serve", "reset"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(stderr.trim().to_string());
        }
    }
    invalidate();
    Ok(())
}

fn read_serve_url() -> Option<String> {
    crate::which("tailscale")?;
    let tailnet = status()?;
    let output = std::process::Command::new("tailscale")
        .args(["serve", "status", "--json"])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            let plain = std::process::Command::new("tailscale")
                .args(["serve", "status"])
                .output()
                .ok()?;
            return parse_serve_plain(std::str::from_utf8(&plain.stdout).ok()?, &tailnet.name);
        }
    };
    parse_serve_json(std::str::from_utf8(&output.stdout).ok()?, &tailnet.name)
}

fn parse_serve_json(json: &str, name: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tcp = v.as_object()?.get("TCP")?;
    tcp.as_object()?.get(":443")?;
    Some(format!("https://{name}"))
}

fn parse_serve_plain(plain: &str, name: &str) -> Option<String> {
    if plain.contains("https://") && plain.contains(":443") {
        Some(format!("https://{name}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tailscale_json() {
        let json = r#"{
            "BackendState": "Running",
            "Self": {
                "DNSName": "machine.tail-scale.ts.net.",
                "TailscaleIPs": [
                    "fd7a:115c:a1e0::1",
                    "100.64.0.1"
                ]
            }
        }"#;
        let result = parse(json);
        assert_eq!(
            result,
            Some(Tailnet {
                state: "Running".to_string(),
                name: "machine.tail-scale.ts.net".to_string(),
                ip: Some(Ipv4Addr::new(100, 64, 0, 1)),
            })
        );
    }
}
