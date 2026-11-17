//! Keeps configured devices bound to `usbip-host` as they come and go.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;

use anyhow::Result;
use farplug_usbip::{DeviceInfo, ExportedDevice, ServerInfo};
use tracing::{info, warn};

use crate::config::{Config, ExportCfg};
use crate::sysfs::{StubStatus, SysDevice, Sysfs};

pub struct Exports {
    cfg: Config,
    pub sysfs: Sysfs,
    /// Export id -> the device currently bound for it.
    bound: BTreeMap<String, SysDevice>,
    /// Last problem logged per export id, so a persistent one is logged once.
    problems: HashMap<String, String>,
    warned_strays: bool,
    /// Busid -> the client that last imported it.
    attached_by: HashMap<String, SocketAddr>,
}

impl Exports {
    pub fn new(cfg: Config, sysfs: Sysfs) -> Self {
        Self {
            cfg,
            sysfs,
            bound: BTreeMap::new(),
            problems: HashMap::new(),
            warned_strays: false,
            attached_by: HashMap::new(),
        }
    }

    /// Apply an edited config. Exports whose entry is gone or no longer
    /// matches go back to the host; the rest stay bound, so their clients
    /// stay connected. New entries are bound by the next [`Self::reconcile`].
    pub fn set_config(&mut self, cfg: Config) {
        for (id, d) in std::mem::take(&mut self.bound) {
            match cfg.device.get(&id) {
                Some(want) if want.matches(&d) => {
                    self.bound.insert(id, d);
                }
                _ => {
                    info!(device = %id, busid = %d.busid, "no longer in the config; returning it to the host");
                    if let Err(e) = self.sysfs.release(&d.busid) {
                        warn!(busid = %d.busid, error = %e, "could not return device to the host");
                    }
                }
            }
        }
        self.problems.retain(|id, _| cfg.device.contains_key(id));
        self.cfg = cfg;
    }

    pub fn reconcile(&mut self) -> Result<()> {
        let devices = self.sysfs.scan()?;
        let by_busid: HashMap<&str, &SysDevice> =
            devices.iter().map(|d| (d.busid.as_str(), d)).collect();

        let ids: Vec<String> = self.bound.keys().cloned().collect();
        for id in ids {
            let busid = self.bound[&id].busid.clone();
            match by_busid.get(busid.as_str()) {
                None => {
                    info!(device = %id, %busid, "unplugged");
                    self.sysfs.forget(&busid);
                    self.bound.remove(&id);
                }
                Some(d) if !self.cfg.device[&id].matches(d) => {
                    info!(device = %id, %busid, "a different device is now on this port");
                    if let Err(e) = self.sysfs.release(&busid) {
                        warn!(%busid, error = %e, "could not return device to the host");
                    }
                    self.bound.remove(&id);
                }
                Some(d) => {
                    self.bound.insert(id, (*d).clone());
                }
            }
        }

        let mut claimed: HashMap<String, String> = self
            .bound
            .iter()
            .map(|(id, d)| (d.busid.clone(), id.clone()))
            .collect();
        let mut newly_bound = Vec::new();
        for (id, want) in &self.cfg.device {
            if self.bound.contains_key(id) {
                continue;
            }
            let problem = match candidates(want, &devices).as_slice() {
                [] => Some("not plugged in".to_owned()),
                [d] => match claimed.get(d.busid.as_str()) {
                    Some(other) => Some(format!("{} is already exported as {other:?}", d.busid)),
                    None => match self.sysfs.bind(d) {
                        Ok(()) => {
                            info!(
                                device = %id,
                                busid = %d.busid,
                                usb_id = %format!("{:04x}:{:04x}", d.vendor_id, d.product_id),
                                "exporting {}",
                                want.label.clone().unwrap_or_else(|| d.description())
                            );
                            claimed.insert(d.busid.clone(), id.clone());
                            newly_bound.push((id.clone(), (*d).clone()));
                            None
                        }
                        Err(e) => Some(format!("binding {} to usbip-host failed: {e:#}", d.busid)),
                    },
                },
                many => Some(format!(
                    "{} devices match ({}); add a serial or port matcher",
                    many.len(),
                    many.iter()
                        .map(|d| d.busid.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            };
            match problem {
                Some(p) => {
                    if self.problems.get(id) != Some(&p) {
                        warn!(device = %id, "{p}");
                        self.problems.insert(id.clone(), p);
                    }
                }
                None => {
                    self.problems.remove(id);
                }
            }
        }
        self.bound.extend(newly_bound);

        if !self.warned_strays {
            self.warned_strays = true;
            for d in devices.iter().filter(|d| d.is_bound_to_usbip()) {
                if !claimed.contains_key(&d.busid) {
                    warn!(
                        busid = %d.busid,
                        "bound to usbip-host but not in the config; \
                         `farplugd release {}` gives it back to the host",
                        d.busid
                    );
                }
            }
        }
        Ok(())
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn list(&self) -> Vec<ExportedDevice> {
        self.bound.iter().map(|(id, d)| d.exported(id)).collect()
    }

    pub fn record_attach(&mut self, busid: &str, peer: SocketAddr) {
        self.attached_by.insert(busid.to_owned(), peer);
    }

    pub fn info(&self) -> ServerInfo {
        let devices = self
            .bound
            .iter()
            .map(|(id, d)| {
                let in_use = matches!(self.sysfs.stub_status(&d.busid), Ok(StubStatus::Used));
                DeviceInfo {
                    id: id.clone(),
                    label: self.cfg.device.get(id).and_then(|c| c.label.clone()),
                    description: d.description(),
                    in_use,
                    used_by: self
                        .attached_by
                        .get(&d.busid)
                        .filter(|_| in_use)
                        .map(|a| a.ip().to_string()),
                }
            })
            .collect();
        ServerInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            devices,
        }
    }

    /// Resolve what a client asked for: an export id, or for compatibility with
    /// plain `usbip attach -b`, the real busid of an exported device.
    pub fn find(&self, name: &str) -> Option<&SysDevice> {
        self.bound
            .get(name)
            .or_else(|| self.bound.values().find(|d| d.busid == name))
    }
}

/// The devices a config entry would export; hubs never qualify.
pub fn candidates<'a>(want: &ExportCfg, devices: &'a [SysDevice]) -> Vec<&'a SysDevice> {
    devices
        .iter()
        .filter(|d| want.matches(d) && !d.is_hub())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::fake::*;
    use crate::sysfs::USBIP_HOST;

    fn exports(fs_: &FakeSys, cfg: &str) -> Exports {
        Exports::new(Config::parse(cfg).unwrap(), fs_.sysfs())
    }

    const ZIGBEE: &str = "[device.zigbee]\nvid = \"10c4\"\npid = \"ea60\"\n";

    #[test]
    fn binds_matching_device_and_lists_it_by_id() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        fs_.add(FakeDev {
            busid: "1-2",
            vid: "046d",
            pid: "c52b",
            ..Default::default()
        });
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/bind"), "1-1");

        let list = ex.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].device.busid, "zigbee");
        assert_eq!(ex.find("zigbee").unwrap().busid, "1-1");
        assert_eq!(ex.find("1-1").unwrap().busid, "1-1");
        assert!(ex.find("1-2").is_none());
    }

    #[test]
    fn reload_keeps_unchanged_exports_and_releases_removed_ones() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        fs_.add(FakeDev {
            busid: "1-2",
            vid: "1a86",
            pid: "55d4",
            ..Default::default()
        });
        let both = format!("{ZIGBEE}[device.zwave]\nvid = \"1a86\"\npid = \"55d4\"\n");
        let mut ex = exports(&fs_, &both);
        ex.reconcile().unwrap();
        assert_eq!(ex.list().len(), 2);

        ex.set_config(Config::parse(ZIGBEE).unwrap());
        ex.reconcile().unwrap();
        let ids: Vec<String> = ex.list().into_iter().map(|d| d.device.busid).collect();
        assert_eq!(ids, ["zigbee"]);
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/rebind"), "1-2");
    }

    #[test]
    fn ambiguous_match_is_not_exported() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        fs_.add(FakeDev {
            busid: "1-2",
            ..Default::default()
        });
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();
        assert!(ex.list().is_empty());
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/bind"), "");
    }

    #[test]
    fn unplug_forgets_port_and_replug_rebinds() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();
        fs_.set_driver("1-1", USBIP_HOST);

        fs_.remove("1-1");
        ex.reconcile().unwrap();
        assert!(ex.list().is_empty());
        assert_eq!(
            fs_.read("bus/usb/drivers/usbip-host/match_busid"),
            "del 1-1"
        );

        fs_.add(FakeDev {
            busid: "1-3",
            ..Default::default()
        });
        ex.reconcile().unwrap();
        assert_eq!(ex.find("zigbee").unwrap().busid, "1-3");
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/bind"), "1-3");
    }

    #[test]
    fn different_device_on_same_port_is_released() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();

        fs_.remove("1-1");
        fs_.add(FakeDev {
            pid: "0000",
            driver: Some(USBIP_HOST),
            ..Default::default()
        });
        ex.reconcile().unwrap();
        assert!(ex.list().is_empty());
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/rebind"), "1-1");
    }

    #[test]
    fn hubs_are_never_exported() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev {
            class: "09",
            ..Default::default()
        });
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();
        assert!(ex.list().is_empty());
    }

    #[test]
    fn adopts_device_already_bound_to_usbip_host() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev {
            driver: Some(USBIP_HOST),
            ..Default::default()
        });
        let mut ex = exports(&fs_, ZIGBEE);
        ex.reconcile().unwrap();
        assert_eq!(ex.list().len(), 1);
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/bind"), "");
    }
}
