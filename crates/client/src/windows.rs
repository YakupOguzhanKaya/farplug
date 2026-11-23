//! Windows backend: the usbip-win2 VHCI driver, which opens the connection to
//! the server itself. We only tell it where to connect.

use std::ffi::c_void;
use std::io;
use std::ptr;

use anyhow::{bail, Context, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_SUCCESS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::winabi::{self, Imported};
use crate::{Attached, Backend, Server};

const INSTALL_HINT: &str = "install usbip-win2 0.9.8.1 from \
                            https://github.com/vadimgrn/usbip-win2/releases";

pub struct UsbipWin2;

impl UsbipWin2 {
    pub fn new() -> Result<Self> {
        Vhci::open()?;
        Ok(Self)
    }
}

impl Backend for UsbipWin2 {
    fn attach(&self, server: &Server, id: &str) -> Result<Attached> {
        let vhci = Vhci::open()?;
        // The driver keeps devices attached when the agent exits without
        // detaching (killed, or replaced during an upgrade); take those over
        // instead of asking the server for a device it says is busy.
        let service = server.port.to_string();
        if let Ok(devs) = vhci.imported() {
            if let Some(d) = devs
                .iter()
                .find(|d| d.busid == id && d.service == service && d.host == server.host)
            {
                return Ok(Attached {
                    port: d.port,
                    devid: 0,
                });
            }
        }
        let req = winabi::plugin_request(&server.host, server.port, id)?;
        let port = vhci.plugin(req)?;
        Ok(Attached { port, devid: 0 })
    }

    fn is_attached(&self, a: &Attached, id: &str) -> bool {
        Vhci::open()
            .and_then(|v| v.imported())
            .map(|devs| devs.iter().any(|d| d.port == a.port && d.busid == id))
            .unwrap_or(false)
    }

    fn detach(&self, a: &Attached) -> Result<()> {
        Vhci::open()?
            .ioctl(
                winabi::IOCTL_PLUGOUT_HARDWARE,
                &winabi::plugout_request(a.port),
                &mut [],
            )
            .map(|_| ())
            .with_context(|| format!("detaching port {}", a.port))
    }
}

/// An open handle to the VHCI device. Opened per operation so nothing
/// non-`Send` is shared between the per-device threads.
struct Vhci(HANDLE);

impl Drop for Vhci {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Vhci {
    fn open() -> Result<Self> {
        let guid = GUID::from_u128(winabi::VHCI_INTERFACE_GUID);
        let mut len = 0u32;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &guid,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            bail!("listing usbip-win2 devices failed (CONFIGRET {cr}); {INSTALL_HINT}");
        }
        let mut buf = vec![0u16; len as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &guid,
                ptr::null(),
                buf.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            bail!("listing usbip-win2 devices failed (CONFIGRET {cr}); {INSTALL_HINT}");
        }
        // A REG_MULTI_SZ-style list; the driver creates exactly one interface.
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if end == 0 {
            bail!("the usbip-win2 driver is not installed or not running; {INSTALL_HINT}");
        }
        let mut path = buf[..end].to_vec();
        path.push(0);

        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error()).context("opening the usbip-win2 driver");
        }
        Ok(Self(h))
    }

    /// Returns the number of bytes the driver wrote to `out`.
    fn ioctl(&self, code: u32, input: &[u8], out: &mut [u8]) -> io::Result<usize> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.as_ptr() as *const c_void,
                input.len() as u32,
                if out.is_empty() {
                    ptr::null_mut()
                } else {
                    out.as_mut_ptr() as *mut c_void
                },
                out.len() as u32,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(returned as usize)
    }

    /// Blocks until the driver has connected, imported and plugged the device.
    fn plugin(&self, mut req: Vec<u8>) -> Result<u32> {
        let input = req.clone();
        let n = self
            .ioctl(
                winabi::IOCTL_PLUGIN_HARDWARE_ONCE,
                &input,
                &mut req[..winabi::PLUGIN_REPLY_LEN],
            )
            .map_err(explain)?;
        winabi::plugin_reply_port(&req[..n])
    }

    fn imported(&self) -> Result<Vec<Imported>> {
        let mut n = 4;
        loop {
            let mut buf = winabi::imported_request(n);
            let input = buf[..4].to_vec();
            match self.ioctl(winabi::IOCTL_GET_IMPORTED_DEVICES, &input, &mut buf) {
                Ok(len) => return winabi::parse_imported(&buf[..len]),
                Err(e)
                    if e.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32) && n < 1024 =>
                {
                    n *= 2
                }
                Err(e) => return Err(explain(e)).context("listing attached devices"),
            }
        }
    }
}

/// The driver reports USB/IP refusals and its own ABI checks as NTSTATUS
/// values with the customer bit set, which reach GetLastError unchanged and
/// have no Windows message text.
fn explain(e: io::Error) -> anyhow::Error {
    match e.raw_os_error() {
        // ERROR_INSUFFICIENT_BUFFER / ERROR_INVALID_USER_BUFFER from the plugin
        // IOCTL mean the installed usbip-win2 ABI does not match this build.
        Some(code) if code == ERROR_INSUFFICIENT_BUFFER as i32 || code == 1784 => {
            anyhow::anyhow!("usbip-win2 ABI mismatch (os error {code}); {INSTALL_HINT}")
        }
        Some(code) if (code as u32) & 0x2000_0000 != 0 => anyhow::anyhow!(
            "usbip-win2 driver error {:#010x} (the server refused, or this usbip-win2 \
             version is not compatible; {INSTALL_HINT})",
            code as u32
        ),
        _ => e.into(),
    }
}
