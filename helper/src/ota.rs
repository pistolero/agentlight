//! Firmware push over an already authorized, certificate-pinned Wi-Fi session.
use crate::wifi::{read_json, write_json};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    net::TcpStream,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Size of each OTA app partition (app0/app1 in default_8MB.csv).
const APP_PARTITION: usize = 0x330000;
const MARKER: &[u8] = b"AGENTLIGHT-BUILD:";
const MAX_ATTEMPTS: u32 = 2;

pub struct OtaImage {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub version: String,
    pub build: String,
}

pub fn load(path: &Path) -> Result<OtaImage> {
    let bytes =
        fs::read(path).with_context(|| format!("read firmware image {}", path.display()))?;
    parse(bytes).with_context(|| {
        format!(
            "{} is not a usable agentlight firmware image",
            path.display()
        )
    })
}

fn parse(bytes: Vec<u8>) -> Result<OtaImage> {
    if bytes.len() < 24 || bytes[0] != 0xE9 {
        bail!("not an ESP32 application image (use firmware.bin, not a merged or ELF file)");
    }
    if u16::from_le_bytes([bytes[12], bytes[13]]) != 9 {
        bail!("image is not built for the ESP32-S3");
    }
    if bytes.len() > APP_PARTITION {
        bail!(
            "image is {} bytes; the display's app slot holds {APP_PARTITION}",
            bytes.len()
        );
    }
    let start = bytes
        .windows(MARKER.len())
        .position(|w| w == MARKER)
        .context("no AGENTLIGHT-BUILD marker; build it from this repository's firmware")?
        + MARKER.len();
    let end = bytes[start..]
        .iter()
        .position(|b| *b == 0)
        .map(|n| start + n)
        .context("unterminated build marker")?;
    let build = std::str::from_utf8(&bytes[start..end])
        .ok()
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .context("invalid build marker")?
        .to_string();
    let version = build.split(';').next().unwrap_or_default().to_string();
    let sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    Ok(OtaImage {
        bytes,
        sha256,
        version,
        build,
    })
}

fn attempts() -> &'static Mutex<HashMap<String, u32>> {
    static A: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// Decide from the device's auth frame whether to push this image now.
pub fn should_update(image: &OtaImage, auth: &Value, device: &str) -> bool {
    let build = auth["build"].as_str().unwrap_or("");
    if build == image.build {
        return false;
    }
    let mut tries = attempts().lock().unwrap();
    let count = tries.entry(device.to_string()).or_insert(0);
    if auth["ota"] != 1 {
        if *count == 0 {
            eprintln!("Display {device} runs firmware without Wi-Fi update support; flash it once over USB.");
            *count = MAX_ATTEMPTS;
        }
        return false;
    }
    if *count >= MAX_ATTEMPTS {
        if *count == MAX_ATTEMPTS {
            eprintln!(
                "Display {device}: giving up on firmware {} after {MAX_ATTEMPTS} attempts.",
                image.version
            );
            *count += 1;
        }
        return false;
    }
    *count += 1;
    true
}

pub fn progress_line(device: &str, sent: usize, total: usize, rate: f64) -> String {
    const WIDTH: usize = 24;
    let total = total.max(1);
    let filled = sent * WIDTH / total;
    let bar: String = "█".repeat(filled) + &"░".repeat(WIDTH - filled);
    let speed = if rate > 0.0 {
        format!("  {:.0} KB/s", rate / 1024.0)
    } else {
        String::new()
    };
    format!(
        "Updating {device}  {bar} {:>3}%  {}/{} KB{speed}",
        sent * 100 / total,
        sent / 1024,
        total / 1024
    )
}

fn wait_for<S: std::io::Read>(
    stream: &mut S,
    pending: &mut Vec<u8>,
    kind: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(v) = read_json(stream, pending)? {
            if v["v"] != 1 {
                continue;
            }
            if v["t"] == "ota_result" && v["ok"] != true {
                bail!(
                    "display rejected the update: {}",
                    v["error"].as_str().unwrap_or("unknown error")
                );
            }
            if v["t"] == kind {
                return Ok(v);
            }
        }
    }
    bail!("display did not answer {kind} in time")
}

/// Run one update. The display reboots on success, closing the connection.
pub fn push(
    tls: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
    image: &OtaImage,
    device: &str,
    current: &str,
) -> Result<()> {
    eprintln!(
        "Display {device}: updating firmware {} -> {} ({} KB)",
        if current.is_empty() {
            "unknown"
        } else {
            current
        },
        image.version,
        image.bytes.len() / 1024
    );
    let mut pending = vec![];
    write_json(
        tls,
        &json!({"v":1,"t":"ota_begin","size":image.bytes.len(),"sha256":image.sha256,"version":image.version}),
    )?;
    wait_for(tls, &mut pending, "ota_ready", Duration::from_secs(30))?;
    // Flash sector erases can stall the receiver; let TCP backpressure absorb it.
    tls.sock.set_write_timeout(Some(Duration::from_secs(20)))?;
    let started = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(1);
    let total = image.bytes.len();
    let mut sent = 0;
    let mut err = std::io::stderr();
    for chunk in image.bytes.chunks(4096) {
        tls.write_all(chunk).context("send firmware")?;
        sent += chunk.len();
        if last.elapsed() >= Duration::from_millis(250) || sent == total {
            let rate = sent as f64 / started.elapsed().as_secs_f64().max(0.001);
            let _ = write!(err, "\r{}", progress_line(device, sent, total, rate));
            last = Instant::now();
        }
    }
    tls.flush()?;
    let _ = writeln!(err);
    eprintln!("Display {device}: verifying and installing...");
    wait_for(tls, &mut pending, "ota_result", Duration::from_secs(60))?;
    eprintln!(
        "Display {device}: installed firmware {}; display is restarting.",
        image.version
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(chip: u16, build: &str) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[0] = 0xE9;
        b[12..14].copy_from_slice(&chip.to_le_bytes());
        b.extend_from_slice(MARKER);
        b.extend_from_slice(build.as_bytes());
        b.push(0);
        b
    }
    #[test]
    fn parses_marker_and_rejects_bad_images() {
        let img = parse(image(9, "0.3.0;Nov  5 2026 18:56:04")).unwrap();
        assert_eq!(img.version, "0.3.0");
        assert_eq!(img.build, "0.3.0;Nov  5 2026 18:56:04");
        assert_eq!(img.sha256.len(), 64);
        let mut bad = image(9, "x");
        bad[0] = 0x7f;
        assert!(parse(bad).is_err());
        assert!(parse(image(0, "x")).is_err());
        let mut big = image(9, "x");
        big.resize(APP_PARTITION + 1, 0);
        assert!(parse(big).is_err());
        let mut none = image(9, "");
        none.truncate(64);
        assert!(parse(none).is_err());
    }
    #[test]
    fn updates_only_differing_capable_displays_with_retry_limit() {
        let img = parse(image(9, "0.3.0;b")).unwrap();
        assert!(!should_update(
            &img,
            &json!({"ota":1,"build":"0.3.0;b"}),
            "same"
        ));
        assert!(!should_update(&img, &json!({"build":"0.2.0"}), "legacy"));
        for _ in 0..MAX_ATTEMPTS {
            assert!(should_update(
                &img,
                &json!({"ota":1,"build":"0.2.0;a"}),
                "retry"
            ));
        }
        assert!(!should_update(
            &img,
            &json!({"ota":1,"build":"0.2.0;a"}),
            "retry"
        ));
    }
    #[test]
    fn progress_line_is_readable() {
        let line = progress_line("dev", 512 * 1024, 1024 * 1024, 200.0 * 1024.0);
        assert!(line.contains(" 50%"));
        assert!(line.contains("512/1024 KB"));
        assert!(line.contains("200 KB/s"));
        assert_eq!(line.matches('█').count(), 12);
    }
}
