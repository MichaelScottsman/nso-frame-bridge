//! Keeps a WinRT "throughput optimized" connection-parameter request alive.
//!
//! btleplug issues the same request but drops the returned
//! `BluetoothLEPreferredConnectionParametersRequest` immediately; Windows only
//! honours the request while that object is open, so the link falls back to
//! the default interval (~15 ms → ~60 Hz input).

use anyhow::{Context, Result};
use windows::Devices::Bluetooth::{
    BluetoothLEDevice, BluetoothLEPreferredConnectionParameters, BluetoothLEPreferredConnectionParametersRequest,
};

pub struct FastLink {
    device: BluetoothLEDevice,
    _request: BluetoothLEPreferredConnectionParametersRequest,
}

/// Connection interval in milliseconds (WinRT reports 1.25 ms units).
pub fn interval_ms(device: &BluetoothLEDevice) -> Option<f32> {
    let p = device.GetConnectionParameters().ok()?;
    Some(p.ConnectionInterval().ok()? as f32 * 1.25)
}

impl FastLink {
    /// `addr` is the controller address, most-significant byte first.
    pub fn request(addr: [u8; 6]) -> Result<Self> {
        let addr = addr.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(addr)?
            .join()
            .context("open BluetoothLEDevice")?;
        let params = BluetoothLEPreferredConnectionParameters::ThroughputOptimized()?;
        let request = device.RequestPreferredConnectionParameters(&params)?;
        let status = request.Status()?;
        anyhow::ensure!(status.0 == 1, "connection parameter request status {}", status.0);
        Ok(Self { device, _request: request })
    }

    pub fn interval_ms(&self) -> Option<f32> {
        interval_ms(&self.device)
    }
}
