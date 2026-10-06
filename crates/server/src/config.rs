use std::{collections::HashMap, path::Path};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use usbridge_proto::{FlowControl, Parity, Profile, SerialParams};

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub device: HashMap<String, DeviceCfg>,
}

fn default_name() -> String {
    "usbridged".into()
}

fn default_listen() -> String {
    format!("0.0.0.0:{}", usbridge_proto::DEFAULT_PORT)
}

#[derive(Debug, Deserialize)]
pub struct DeviceCfg {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "default_profile")]
    pub profile: ProfileCfg,

    /// Matchers. At least one must be set; all that are set must match.
    #[serde(default)]
    pub vid: Option<String>,
    #[serde(default)]
    pub pid: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub path: Option<String>,

    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    #[serde(default)]
    pub parity: ParityCfg,
    #[serde(default)]
    pub flow_control: FlowCfg,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileCfg {
    Serial,
}

fn default_profile() -> ProfileCfg {
    ProfileCfg::Serial
}

fn default_baud() -> u32 {
    115_200
}

fn default_data_bits() -> u8 {
    8
}

fn default_stop_bits() -> u8 {
    1
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParityCfg {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowCfg {
    #[default]
    None,
    Software,
    Hardware,
}

impl DeviceCfg {
    pub fn profile(&self) -> Profile {
        match self.profile {
            ProfileCfg::Serial => Profile::Serial,
        }
    }

    pub fn params(&self) -> SerialParams {
        SerialParams {
            baud: self.baud,
            data_bits: self.data_bits,
            stop_bits: self.stop_bits,
            parity: match self.parity {
                ParityCfg::None => Parity::None,
                ParityCfg::Odd => Parity::Odd,
                ParityCfg::Even => Parity::Even,
            },
            flow_control: match self.flow_control {
                FlowCfg::None => FlowControl::None,
                FlowCfg::Software => FlowControl::Software,
                FlowCfg::Hardware => FlowControl::Hardware,
            },
        }
    }

    pub fn vid_u16(&self) -> Result<Option<u16>> {
        parse_hex16(self.vid.as_deref(), "vid")
    }

    pub fn pid_u16(&self) -> Result<Option<u16>> {
        parse_hex16(self.pid.as_deref(), "pid")
    }
}

fn parse_hex16(s: Option<&str>, field: &str) -> Result<Option<u16>> {
    let Some(s) = s else { return Ok(None) };
    let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(trimmed, 16)
        .map(Some)
        .with_context(|| format!("{field} must be 4 hex digits, got {s:?}"))
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).context("parsing config")?;

    for (id, dev) in &cfg.device {
        if dev.vid.is_none() && dev.pid.is_none() && dev.serial.is_none() && dev.path.is_none() {
            bail!("device {id:?} has no matcher: set at least one of vid/pid/serial/path");
        }
        dev.vid_u16()?;
        dev.pid_u16()?;
    }
    Ok(cfg)
}
