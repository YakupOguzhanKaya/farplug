use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use farplug_usbip::{DEFAULT_PORT, MAX_BUSID};
use serde::Deserialize;

use crate::sysfs::SysDevice;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub device: BTreeMap<String, ExportCfg>,
    /// Also export every device no [device.*] entry matches, under a
    /// generated id. Hubs, network adapters and storage are never included.
    #[serde(default)]
    pub export_all: bool,
    /// Ports ("1-1.4") or USB ids ("046d:c548") that export_all leaves alone.
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_listen() -> String {
    format!("0.0.0.0:{DEFAULT_PORT}")
}

/// One exported device. Every matcher that is set must match.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportCfg {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, deserialize_with = "hex16")]
    pub vid: Option<u16>,
    #[serde(default, deserialize_with = "hex16")]
    pub pid: Option<u16>,
    #[serde(default)]
    pub serial: Option<String>,
    /// Physical port as a sysfs busid, e.g. "1-1.4", for identical devices
    /// that have no serial number.
    #[serde(default)]
    pub port: Option<String>,
}

impl ExportCfg {
    pub fn matches(&self, d: &SysDevice) -> bool {
        self.vid.is_none_or(|v| v == d.vendor_id)
            && self.pid.is_none_or(|p| p == d.product_id)
            && self
                .serial
                .as_ref()
                .is_none_or(|s| d.serial.as_ref() == Some(s))
            && self.port.as_ref().is_none_or(|p| *p == d.busid)
    }
}

fn hex16<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    s.map(|s| {
        let t = s.trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(t, 16)
            .map_err(|_| serde::de::Error::custom(format!("{s:?} is not a 4-digit hex id")))
    })
    .transpose()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        for (id, dev) in &cfg.device {
            if id.is_empty() || id.len() > MAX_BUSID {
                bail!("device id {id:?} must be 1 to {MAX_BUSID} characters");
            }
            if !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                bail!("device id {id:?} may only contain letters, digits, '-', '_' and '.'");
            }
            if dev.vid.is_none() && dev.pid.is_none() && dev.serial.is_none() && dev.port.is_none()
            {
                bail!("[device.{id}] needs at least one of vid, pid, serial, port");
            }
        }
        for e in &cfg.exclude {
            let usb_id = e.split_once(':').is_some_and(|(v, p)| {
                [v, p]
                    .iter()
                    .all(|h| h.len() == 4 && h.chars().all(|c| c.is_ascii_hexdigit()))
            });
            let port = !e.contains(':')
                && !e.is_empty()
                && e.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | '.'));
            if !usb_id && !port {
                bail!("exclude entry {e:?} must be a port like \"1-1.4\" or a USB id like \"046d:c548\"");
            }
        }
        Ok(cfg)
    }

    pub fn excluded(&self, d: &SysDevice) -> bool {
        let usb_id = format!("{:04x}:{:04x}", d.vendor_id, d.product_id);
        self.exclude
            .iter()
            .any(|e| *e == d.busid || e.eq_ignore_ascii_case(&usb_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let cfg = Config::parse(
            r#"
            [device.zigbee]
            vid = "10c4"
            pid = "0xEA60"
            serial = "0001"

            [device.printer]
            port = "1-1.4"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:3240");
        assert_eq!(cfg.device["zigbee"].pid, Some(0xea60));
        assert_eq!(cfg.device["printer"].port.as_deref(), Some("1-1.4"));

        assert!(Config::parse("[device.x]\nlabel = \"no matcher\"").is_err());
        assert!(Config::parse("[device.\"has space\"]\nvid = \"1234\"").is_err());
        assert!(Config::parse("[device.x]\nvid = \"zz\"").is_err());
        assert!(Config::parse("[device.x]\nbaud = 9600").is_err());
        let cfg = Config::parse("export_all = true\nexclude = [\"1-1.4\", \"046D:C548\"]").unwrap();
        assert!(cfg.export_all);
        assert!(Config::parse("exclude = [\"usb stick\"]").is_err());
    }
}
