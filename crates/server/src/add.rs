//! `farplugd add`: pick plugged-in devices and append config entries for them.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::exports::{candidates, slug};
use crate::sysfs::{SysDevice, Sysfs};
use crate::table;

pub fn run(
    path: &Path,
    sysfs: &Sysfs,
    ports: &[String],
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let cfg = Config::parse(&existing).with_context(|| format!("in {}", path.display()))?;
    let devices = sysfs.scan()?;
    let exported: BTreeSet<&str> = cfg
        .device
        .values()
        .flat_map(|want| candidates(want, &devices))
        .map(|d| d.busid.as_str())
        .collect();
    let free: Vec<&SysDevice> = devices
        .iter()
        .filter(|d| !d.is_hub() && !exported.contains(d.busid.as_str()))
        .collect();

    let chosen: Vec<&SysDevice> = if ports.is_empty() {
        if free.is_empty() {
            writeln!(out, "every plugged-in device is already in the config")?;
            return Ok(());
        }
        pick(&free, input, out)?
    } else {
        ports
            .iter()
            .map(|p| match free.iter().find(|d| d.busid == *p) {
                Some(d) => Ok(*d),
                None if devices.iter().any(|d| d.busid == *p) => {
                    bail!("{p} is a hub or already in the config")
                }
                None => bail!("nothing is plugged into port {p}"),
            })
            .collect::<Result<_>>()?
    };
    if chosen.is_empty() {
        return Ok(());
    }

    let mut taken: BTreeSet<String> = cfg.device.keys().cloned().collect();
    let mut blocks = String::new();
    for d in chosen {
        writeln!(
            out,
            "\n{} {:04x}:{:04x} {}",
            d.busid,
            d.vendor_id,
            d.product_id,
            d.description()
        )?;
        let id = ask_id(&suggest_id(d, &taken), &taken, input, out)?;
        let label = ask(&format!("label [{}]: ", d.description()), input, out)?
            .unwrap_or_else(|| d.description());
        taken.insert(id.clone());
        blocks.push_str(&entry(&id, &label, d, &devices));
    }

    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&blocks);
    Config::parse(&text).context("the new entries do not form a valid config")?;

    writeln!(out, "\nThis will be added to {}:\n{blocks}", path.display())?;
    if !matches!(
        ask("Write it? [Y/n] ", input, out)?.as_deref(),
        None | Some("y" | "Y" | "yes")
    ) {
        writeln!(out, "nothing written")?;
        return Ok(());
    }
    write_atomic(path, &text)?;
    writeln!(
        out,
        "written. A running farplugd applies it within a second; `farplugd check` shows what matched."
    )?;
    Ok(())
}

fn pick<'a>(
    free: &[&'a SysDevice],
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Vec<&'a SysDevice>> {
    let mut rows = vec![table::row(["#", "PORT", "USB ID", "SERIAL", "DESCRIPTION"])];
    for (i, d) in free.iter().enumerate() {
        rows.push(vec![
            (i + 1).to_string(),
            d.busid.clone(),
            format!("{:04x}:{:04x}", d.vendor_id, d.product_id),
            d.serial.clone().unwrap_or_else(|| "-".into()),
            d.description(),
        ]);
    }
    let mut buf = Vec::new();
    table::write(&mut buf, &rows)?;
    out.write_all(&buf)?;
    loop {
        let Some(answer) = ask(
            "Export which? Numbers separated by spaces, Enter to cancel: ",
            input,
            out,
        )?
        else {
            return Ok(Vec::new());
        };
        let picked: Option<Vec<&SysDevice>> = answer
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .map(|n| {
                n.parse::<usize>()
                    .ok()
                    .and_then(|n| free.get(n.wrapping_sub(1)).copied())
            })
            .collect();
        match picked {
            Some(p) => return Ok(p),
            None => writeln!(out, "pick numbers from the # column")?,
        }
    }
}

fn ask_id(
    suggestion: &str,
    taken: &BTreeSet<String>,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<String> {
    loop {
        let id = ask(&format!("id [{suggestion}]: "), input, out)?
            .unwrap_or_else(|| suggestion.to_owned());
        if taken.contains(&id) {
            writeln!(out, "{id} is already used in the config")?;
            continue;
        }
        match Config::parse(&format!("[device.\"{id}\"]\nport = \"x\"\n")) {
            Ok(_) => return Ok(id),
            Err(e) => writeln!(out, "{e:#}")?,
        }
    }
}

/// One trimmed line; None for an empty answer or the end of input.
fn ask(prompt: &str, input: &mut impl BufRead, out: &mut impl Write) -> Result<Option<String>> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let line = line.trim();
    Ok((!line.is_empty()).then(|| line.to_owned()))
}

/// The device's name, shortened: "Silicon Labs CP2102N" -> "silicon-labs-cp2102n".
fn suggest_id(d: &SysDevice, taken: &BTreeSet<String>) -> String {
    let base = match slug(&d.description(), 3, 24) {
        b if b.is_empty() => format!("usb-{:04x}-{:04x}", d.vendor_id, d.product_id),
        b => b,
    };
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|id| !taken.contains(id))
        .expect("an unused id exists")
}

/// A config block whose matchers pick out this device among those plugged in.
fn entry(id: &str, label: &str, d: &SysDevice, all: &[SysDevice]) -> String {
    let twins = all
        .iter()
        .filter(|o| {
            o.busid != d.busid
                && !o.is_hub()
                && o.vendor_id == d.vendor_id
                && o.product_id == d.product_id
        })
        .count();
    let mut s = format!("\n[device.{}]\n", toml_key(id));
    s.push_str(&format!(
        "label = {}\n",
        toml::Value::String(label.to_owned())
    ));
    s.push_str(&format!(
        "vid = \"{:04x}\"\npid = \"{:04x}\"\n",
        d.vendor_id, d.product_id
    ));
    match &d.serial {
        Some(serial) => s.push_str(&format!(
            "serial = {}\n",
            toml::Value::String(serial.clone())
        )),
        // Identical devices without serials can only be told apart by port.
        None if twins > 0 => s.push_str(&format!("port = \"{}\"\n", d.busid)),
        None => {}
    }
    s
}

fn toml_key(id: &str) -> String {
    if id.contains('.') {
        format!("\"{id}\"")
    } else {
        id.to_owned()
    }
}

/// Replaced in one step, so the running server never reads a half-written file.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    if let Ok(m) = fs::metadata(path) {
        fs::set_permissions(&tmp, m.permissions())?;
    }
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::fake::*;

    #[test]
    fn adds_picked_devices_with_distinguishing_matchers() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev {
            busid: "1-1",
            serial: Some("A1"),
            ..Default::default()
        });
        fs_.add(FakeDev {
            busid: "1-2",
            vid: "1a86",
            pid: "55d4",
            ..Default::default()
        });
        fs_.add(FakeDev {
            busid: "1-3",
            vid: "1a86",
            pid: "55d4",
            ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "listen = \"0.0.0.0:3240\"\n").unwrap();

        // Pick 1 and 2; keep the first suggested id, rename the second; default labels; confirm.
        let mut input = &b"1 2\n\n\nzwave\nZooz stick\ny\n"[..];
        let mut out = Vec::new();
        run(&path, &fs_.sysfs(), &[], &mut input, &mut out).unwrap();

        let cfg = Config::load(&path).unwrap();
        let ids: Vec<&str> = cfg.device.keys().map(String::as_str).collect();
        assert_eq!(ids, ["cp2102n-usb-to", "zwave"]);
        let a = &cfg.device["cp2102n-usb-to"];
        assert_eq!(
            (a.vid, a.serial.as_deref(), a.port.as_deref()),
            (Some(0x10c4), Some("A1"), None)
        );
        let z = &cfg.device["zwave"];
        assert_eq!(z.label.as_deref(), Some("Zooz stick"));
        assert_eq!(z.port.as_deref(), Some("1-2"));

        // Already-configured devices are no longer offered.
        let mut input = &b"\n"[..];
        let mut out = Vec::new();
        run(&path, &fs_.sysfs(), &[], &mut input, &mut out).unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("1-3") && !shown.contains(" 1-1 ") && !shown.contains(" 1-2 "));
    }
}
