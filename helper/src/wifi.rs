//! USB-approved, certificate-pinned TLS transport. Discovery never grants access.
use crate::{
    collect, env_path, home, logs,
    ota::{self, OtaImage},
    state, Args,
};
use anyhow::{bail, Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use rand::RngCore;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig, ServerConnection, StreamOwned,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

const PORT: u16 = 47832;
#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    cert: Vec<u8>,
    key: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct Permit {
    device: String,
    token: String,
}
fn directory() -> Result<PathBuf> {
    let p = env_path("XDG_CONFIG_HOME", home().join(".config")).join("agentlight");
    fs::create_dir_all(&p)?;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
    Ok(p)
}
fn save<T: Serialize>(path: &std::path::Path, v: &T) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    f.write_all(&serde_json::to_vec_pretty(v)?)?;
    f.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}
fn random_hex() -> String {
    let mut b = [0; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn identity() -> Result<Identity> {
    let path = directory()?.join("wifi-identity.json");
    if path.exists() {
        return Ok(serde_json::from_slice(&fs::read(path)?)?);
    }
    let id = random_hex()[..16].to_string();
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec![format!("agentlight-{id}.local")])?;
    let identity = Identity {
        id,
        cert: cert.der().to_vec(),
        key: key_pair.serialize_der(),
    };
    save(&path, &identity)?;
    Ok(identity)
}
fn permits() -> Result<Vec<Permit>> {
    let path = directory()?.join("wifi-displays.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn authorized(device: &str, token: &str) -> bool {
    permits()
        .map(|p| {
            p.iter().any(|p| {
                p.device == device && bool::from(p.token.as_bytes().ct_eq(token.as_bytes()))
            })
        })
        .unwrap_or(false)
}
pub fn list() -> Result<()> {
    for p in permits()? {
        println!(
            "{} (approved for status and requested log access)",
            p.device
        );
    }
    Ok(())
}
pub fn revoke(id: &str) -> Result<()> {
    let mut p = permits()?;
    let old = p.len();
    p.retain(|p| p.device != id);
    if p.len() == old {
        bail!("No approved display with ID {id}");
    }
    save(&directory()?.join("wifi-displays.json"), &p)?;
    println!("Revoked {id}. Active access stops within a few seconds.");
    Ok(())
}
pub struct Server {
    mdns: ServiceDaemon,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.mdns.shutdown();
    }
}
pub fn start(demo: bool, image: Option<Arc<OtaImage>>) -> Result<Server> {
    let identity = identity()?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(identity.cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key)),
        )?;
    let listener = TcpListener::bind(("0.0.0.0", PORT)).context("bind Wi-Fi TLS server")?;
    let mdns = ServiceDaemon::new()?;
    let props = [
        ("helper", identity.id.as_str()),
        ("proto", "1"),
        ("tls", "pinned"),
    ];
    let service = ServiceInfo::new(
        "_agentlight._tcp.local.",
        &format!("Agentlight-{}", identity.id),
        &format!("agentlight-{}.local.", identity.id),
        "",
        PORT,
        &props[..],
    )?
    .enable_addr_auto();
    mdns.register(service)?;
    let config = Arc::new(config);
    let active = Arc::new(AtomicUsize::new(0));
    thread::spawn(move || {
        for socket in listener.incoming().flatten() {
            if active.fetch_add(1, Ordering::SeqCst) >= 2 {
                active.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let config = config.clone();
            let active = active.clone();
            let image = image.clone();
            thread::spawn(move || {
                if let Err(error) = serve(socket, config, demo, image.as_deref()) {
                    eprintln!("Wi-Fi connection closed: {error}");
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    eprintln!("Encrypted Wi-Fi server advertised on TCP {PORT}. Only USB-approved displays can access sessions.");
    Ok(Server { mdns })
}
pub(crate) fn write_json<W: Write>(stream: &mut W, v: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(v)?;
    if bytes.len() > 4096 {
        bail!("Wi-Fi frame too large");
    }
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}
// Read without losing partial frames on a socket timeout. Reject oversize input.
pub(crate) fn read_json<R: Read>(stream: &mut R, pending: &mut Vec<u8>) -> Result<Option<Value>> {
    loop {
        let mut b = [0];
        match stream.read(&mut b) {
            Ok(0) => bail!("Disconnected"),
            Ok(_) => {
                if b[0] == b'\n' {
                    let line = std::mem::take(pending);
                    return Ok(serde_json::from_slice(&line).ok());
                }
                if pending.len() >= 512 {
                    bail!("Oversize request");
                }
                pending.push(b[0]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        }
    }
}
fn serve(
    socket: TcpStream,
    config: Arc<ServerConfig>,
    demo: bool,
    image: Option<&OtaImage>,
) -> Result<()> {
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut tls = StreamOwned::new(ServerConnection::new(config)?, socket);
    let mut pending = vec![];
    let deadline = Instant::now() + Duration::from_secs(10);
    let auth = loop {
        if Instant::now() > deadline {
            bail!("Authentication timeout");
        }
        if let Some(v) = read_json(&mut tls, &mut pending)? {
            break v;
        }
    };
    let device = auth["device"].as_str().unwrap_or("");
    let token = auth["token"].as_str().unwrap_or("");
    if auth["v"] != 1 || auth["t"] != "auth" || !authorized(device, token) {
        bail!("Display is not approved");
    }
    let logv = auth["logv"].as_u64().unwrap_or(1);
    write_json(&mut tls, &json!({"v":1,"t":"authorized"}))?;
    eprintln!("Approved Wi-Fi display authenticated: {device}");
    if let Some(image) = image.filter(|i| ota::should_update(i, &auth, device)) {
        return ota::push(&mut tls, image, device, auth["fw"].as_str().unwrap_or(""));
    }
    loop {
        // Reload the allowlist, including revocations, before each update.
        if !authorized(device, token) {
            bail!("Revoked");
        }
        let mut s = state(collect(demo));
        while serde_json::to_vec(&s)?.len() > 4096 && !s.agents.is_empty() {
            if let Some(a) = s.agents.iter_mut().rev().find(|a| !a.children.is_empty()) {
                a.children.pop();
            } else {
                s.agents.pop();
                s.more += 1;
            }
        }
        write_json(&mut tls, &serde_json::to_value(&s)?)?;
        let until = Instant::now() + Duration::from_secs(2);
        let mut answered = false;
        while Instant::now() < until {
            if let Some(request) = read_json(&mut tls, &mut pending)? {
                if answered {
                    continue;
                }
                if let Some(response) = logs::answer(&request, &s, demo, logv) {
                    if !authorized(device, token) {
                        bail!("Revoked");
                    }
                    write_json(&mut tls, &response)?;
                    answered = true;
                }
            }
        }
    }
}
pub fn setup(args: &Args) -> Result<()> {
    let identity = identity()?;
    let mut port = serialport::new(args.port.as_deref().unwrap(), 115200)
        .timeout(Duration::from_millis(300))
        .open()?;
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    let until = Instant::now() + Duration::from_secs(8);
    let mut pending = vec![];
    let device = loop {
        if Instant::now() > until {
            bail!("Display did not identify itself. Flash the Wi-Fi firmware and stop the running USB helper first.");
        }
        port.write_all(b"{\"v\":1,\"t\":\"hello\"}\n")?;
        if let Some(v) = read_json(&mut port, &mut pending)? {
            if v["device"] == "managents" && v["wifi_setup"] == true {
                if let Some(id) = v["device_id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 48)
                {
                    break id.to_owned();
                }
            }
        }
    };
    println!("Display: {device}\nAllow this display REMOTE ACCESS to session names, status, tasks, subagents and requested message logs?\nThis access continues until revoked with --wifi-revoke. Type APPROVE to continue:");
    let mut approval = String::new();
    std::io::stdin().read_line(&mut approval)?;
    if approval.trim() != "APPROVE" {
        bail!("Not approved; no network credentials or authorization were sent.");
    }
    let ssid = if let Some(ssid) = &args.ssid {
        ssid.clone()
    } else {
        println!("2.4 GHz Wi-Fi network name:");
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        s.trim_end().into()
    };
    if ssid.is_empty() || ssid.len() > 32 {
        bail!("Invalid SSID");
    }
    let password = rpassword::prompt_password("Wi-Fi password (hidden; blank for open network): ")?;
    if password.len() > 63 {
        bail!("Wi-Fi password too long");
    }
    let token = random_hex();
    let fingerprint: String = Sha256::digest(&identity.cert)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    // Grant after explicit confirmation, before provisioning; remove on failure.
    let mut allow = permits()?;
    allow.retain(|p| p.device != device);
    allow.push(Permit {
        device: device.clone(),
        token: token.clone(),
    });
    save(&directory()?.join("wifi-displays.json"), &allow)?;
    let config = json!({"v":1,"t":"wifi_config","ssid":ssid,"password":password,"helper":identity.id,"fingerprint":fingerprint,"token":token});
    if let Err(e) = write_json(&mut port, &config) {
        let _ = revoke(&device);
        return Err(e);
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Some(v) = read_json(&mut port, &mut pending)? {
            if v["t"] == "wifi_configured" && v["ok"] == true {
                println!("Approved and provisioned {device}. Start the helper with --wifi; the display discovers it automatically.");
                return Ok(());
            }
        }
    }
    let _ = revoke(&device);
    bail!("No configuration acknowledgement. Authorization revoked; retry setup.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_reader_bounds_and_parses() {
        let mut pending = vec![];
        assert_eq!(
            read_json(&mut &b"{\"v\":1}\n"[..], &mut pending)
                .unwrap()
                .unwrap()["v"],
            1
        );
        assert!(read_json(&mut &vec![b'x'; 514][..], &mut pending).is_err());
    }
    #[test]
    fn random_tokens_are_distinct_and_256_bits() {
        let a = random_hex();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_hex());
    }
}
