//! Keyboards and mice in HID boot protocol: their reports turned into key,
//! button, motion and scroll events for an [`Input`] that replays them on the
//! local machine (macOS: synthetic CoreGraphics events).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{bail, Result};
use tracing::debug;

use crate::userhost::descriptors::{EndpointKind, Interface};
use crate::userhost::Device;

const HID_CLASS: u8 = 3;
const BOOT_SUBCLASS: u8 = 1;
const KEYBOARD: u8 = 1;
const MOUSE: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A key by its HID usage on the keyboard page; modifiers are 0xE0..0xE7.
    Key {
        usage: u8,
        down: bool,
    },
    /// 0 left, 1 right, 2 middle, then extra buttons.
    Button {
        button: u8,
        down: bool,
    },
    Motion {
        dx: i32,
        dy: i32,
    },
    Scroll {
        lines: i32,
    },
}

/// Where events go: the local OS, or a log where there is no injection.
pub trait Input: Send {
    fn send(&mut self, e: Event);
}

/// Boot keyboard reports: modifier bits, a reserved byte, then up to six
/// pressed keys. Events are the differences from the previous report.
#[derive(Default)]
pub struct Keyboard {
    modifiers: u8,
    keys: Vec<u8>,
}

impl Keyboard {
    pub fn update(&mut self, report: &[u8], out: &mut Vec<Event>) {
        if report.len() < 3 {
            return;
        }
        let modifiers = report[0];
        // 0x01..0x03 fill every slot when too many keys are held: ignore.
        let keys: Vec<u8> = report[2..report.len().min(8)]
            .iter()
            .copied()
            .filter(|&k| k > 3)
            .collect();
        if report[2..].iter().any(|&k| (1..=3).contains(&k)) {
            return;
        }
        for bit in 0..8 {
            let (was, is) = (self.modifiers >> bit & 1, modifiers >> bit & 1);
            if was != is {
                out.push(Event::Key {
                    usage: 0xe0 + bit,
                    down: is == 1,
                });
            }
        }
        for &k in self.keys.iter().filter(|k| !keys.contains(k)) {
            out.push(Event::Key {
                usage: k,
                down: false,
            });
        }
        for &k in keys.iter().filter(|k| !self.keys.contains(k)) {
            out.push(Event::Key {
                usage: k,
                down: true,
            });
        }
        self.modifiers = modifiers;
        self.keys = keys;
    }
}

/// Boot mouse reports: buttons, x and y motion, and on most mice a fourth
/// byte with the wheel.
#[derive(Default)]
pub struct Mouse {
    buttons: u8,
}

impl Mouse {
    pub fn update(&mut self, report: &[u8], out: &mut Vec<Event>) {
        if report.len() < 3 {
            return;
        }
        let buttons = report[0];
        for b in 0..5 {
            let (was, is) = (self.buttons >> b & 1, buttons >> b & 1);
            if was != is {
                out.push(Event::Button {
                    button: b,
                    down: is == 1,
                });
            }
        }
        self.buttons = buttons;
        let (dx, dy) = (report[1] as i8, report[2] as i8);
        if dx != 0 || dy != 0 {
            out.push(Event::Motion {
                dx: dx.into(),
                dy: dy.into(),
            });
        }
        if let Some(&w) = report.get(3) {
            if w != 0 {
                out.push(Event::Scroll {
                    lines: (w as i8).into(),
                });
            }
        }
    }
}

/// The keyboard and mouse interfaces of a device, if it has any.
pub fn boot_interfaces(dev: &Device) -> Vec<(Interface, u8)> {
    dev.config
        .interfaces
        .iter()
        .filter(|i| i.class == HID_CLASS && i.subclass == BOOT_SUBCLASS)
        .filter(|i| matches!(i.protocol, KEYBOARD | MOUSE))
        .filter(|i| i.endpoint(EndpointKind::Interrupt, true).is_some())
        .map(|i| (i.clone(), i.protocol))
        .collect()
}

/// Replays a device's keyboards and mice until stopped or disconnected.
pub struct Session {
    dev: Arc<Device>,
    stop: Arc<AtomicBool>,
    pub kinds: String,
}

impl Session {
    pub fn start(dev: Device, input: Box<dyn Input>) -> Result<Self> {
        let ifaces = boot_interfaces(&dev);
        if ifaces.is_empty() {
            bail!("no keyboard or mouse interface");
        }
        for (i, _) in &ifaces {
            let n = u16::from(i.number);
            // SET_PROTOCOL(boot) so reports have the fixed layout parsed here,
            // and SET_IDLE(0) so the device only reports changes. Some devices
            // stall SET_IDLE; that is harmless.
            dev.control_out(0x21, 0x0b, 0, n, &[])?;
            let _ = dev.control_out(0x21, 0x0a, 0, n, &[]);
        }
        let kinds = ifaces
            .iter()
            .map(|(_, p)| if *p == KEYBOARD { "keyboard" } else { "mouse" })
            .collect::<Vec<_>>()
            .join(" and ");
        let dev = Arc::new(dev);
        let stop = Arc::new(AtomicBool::new(false));
        let input = Arc::new(Mutex::new(input));
        for (iface, protocol) in ifaces {
            let ep = iface
                .endpoint(EndpointKind::Interrupt, true)
                .expect("filtered on having one");
            let (d, s, input) = (dev.clone(), stop.clone(), input.clone());
            thread::spawn(move || {
                let (mut kb, mut mouse) = (Keyboard::default(), Mouse::default());
                let mut events = Vec::new();
                while !s.load(Ordering::Relaxed) {
                    let report = match d.read(ep.address, ep.max_packet.into(), None) {
                        Ok(r) => r,
                        Err(e) => {
                            debug!(error = %e, "input read ended");
                            break;
                        }
                    };
                    if protocol == KEYBOARD {
                        kb.update(&report, &mut events);
                    } else {
                        mouse.update(&report, &mut events);
                    }
                    let mut input = input.lock().unwrap();
                    for e in events.drain(..) {
                        input.send(e);
                    }
                }
                s.store(true, Ordering::Relaxed);
                d.close();
            });
        }
        Ok(Self { dev, stop, kinds })
    }

    pub fn is_alive(&self) -> bool {
        !self.stop.load(Ordering::Relaxed) && self.dev.is_alive()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.dev.close();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Where the OS cannot take injected input: log what would have been sent.
pub struct LogInput;

impl Input for LogInput {
    fn send(&mut self, e: Event) {
        tracing::info!(?e, "input");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_reports_become_key_changes() {
        let mut kb = Keyboard::default();
        let mut ev = Vec::new();
        kb.update(&[0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut ev); // shift + a
        assert_eq!(
            ev,
            [
                Event::Key {
                    usage: 0xe1,
                    down: true
                },
                Event::Key {
                    usage: 0x04,
                    down: true
                }
            ]
        );
        ev.clear();
        kb.update(&[0x02, 0, 0x04, 0x05, 0, 0, 0, 0], &mut ev); // + b
        assert_eq!(
            ev,
            [Event::Key {
                usage: 0x05,
                down: true
            }]
        );
        ev.clear();
        kb.update(&[0x01, 0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01], &mut ev); // rollover
        assert!(ev.is_empty());
        kb.update(&[0, 0, 0, 0, 0, 0, 0, 0], &mut ev);
        assert_eq!(
            ev,
            [
                Event::Key {
                    usage: 0xe1,
                    down: false
                },
                Event::Key {
                    usage: 0x04,
                    down: false
                },
                Event::Key {
                    usage: 0x05,
                    down: false
                }
            ]
        );
    }

    #[test]
    fn mouse_reports_become_buttons_motion_and_scroll() {
        let mut m = Mouse::default();
        let mut ev = Vec::new();
        m.update(&[0x01, 5, 0xfe, 0xff], &mut ev);
        assert_eq!(
            ev,
            [
                Event::Button {
                    button: 0,
                    down: true
                },
                Event::Motion { dx: 5, dy: -2 },
                Event::Scroll { lines: -1 }
            ]
        );
        ev.clear();
        m.update(&[0x00, 0, 0], &mut ev);
        assert_eq!(
            ev,
            [Event::Button {
                button: 0,
                down: false
            }]
        );
    }
}
