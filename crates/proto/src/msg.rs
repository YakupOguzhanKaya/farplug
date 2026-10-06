use serde::{Deserialize, Serialize};

/// Stable identifier for an exported device. Derived from the server config key,
/// not from the OS device path, so it survives re-enumeration and reboots.
pub type DeviceId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    Serial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub label: String,
    pub profile: Profile,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial: Option<String>,
    pub present: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialParams {
    pub baud: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: Parity,
    pub flow_control: FlowControl,
}

impl Default for SerialParams {
    fn default() -> Self {
        Self {
            baud: 115_200,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            flow_control: FlowControl::None,
        }
    }
}

/// Modem control lines the client drives. Zigbee coordinators ignore these, but
/// ESP32 and AVR bootloaders are entered by toggling them, so they are part of
/// the data plane rather than connection setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLines {
    pub dtr: bool,
    pub rts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        version: u16,
        client_name: String,
    },
    List,
    /// On success the connection leaves control mode and carries only data-plane
    /// frames for this device until it closes.
    Attach {
        id: DeviceId,
        profile: Profile,
        params: SerialParams,
        /// Take the device from an existing holder instead of being refused.
        steal: bool,
    },
    Data(Vec<u8>),
    SetControlLines(ControlLines),
    SetParams(SerialParams),
    /// Send a serial break for the given duration.
    Break { millis: u32 },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        version: u16,
        server_name: String,
    },
    Devices(Vec<DeviceInfo>),
    Attached {
        id: DeviceId,
        params: SerialParams,
    },
    Data(Vec<u8>),
    /// The physical device disappeared. The client keeps its PTY open and waits,
    /// so applications holding the port do not have to be restarted.
    DeviceGone,
    DeviceBack,
    Error {
        code: ErrorCode,
        detail: String,
    },
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    VersionMismatch,
    NoSuchDevice,
    DeviceBusy,
    DeviceAbsent,
    ProfileMismatch,
    Io,
    Protocol,
}
