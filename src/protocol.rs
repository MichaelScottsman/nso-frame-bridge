//! Switch 2 controller BLE protocol (NSO GameCube focus).
//!
//! Ported from joypad-os (Apache-2.0):
//!   src/bt/btstack/btstack_host.c            — detection, init/pairing sequence, rumble
//!   src/bt/bthid/devices/vendors/nintendo/switch2_ble.c — input report parsing
//!   src/bt/switch2_ble/gen_switch2_gatt.py   — GATT UUIDs
//! which in turn credit BlueRetro and ndeadly/switch2_controller_research.

use uuid::{Uuid, uuid};

/// Bluetooth SIG company ID Nintendo uses in Switch 2 controller adverts.
pub const NINTENDO_COMPANY_ID: u16 = 0x0553;
pub const NINTENDO_VID: u16 = 0x057E;

pub const PID_JOYCON2_L: u16 = 0x2066;
pub const PID_JOYCON2_R: u16 = 0x2067;
pub const PID_PRO2: u16 = 0x2069;
pub const PID_GC: u16 = 0x2073;

pub fn pid_name(pid: u16) -> &'static str {
    match pid {
        PID_JOYCON2_L => "Joy-Con 2 (L)",
        PID_JOYCON2_R => "Joy-Con 2 (R)",
        PID_PRO2 => "Pro Controller 2",
        PID_GC => "NSO GameCube Controller",
        _ => "unknown",
    }
}

/// Input report notifications (ATT handle 0x000A on the Pro Controller 2).
pub const CHR_INPUT: Uuid = uuid!("ab7de9be-89fe-49ad-828f-118f09df7fd2");
/// Vibration output, write without response (handle 0x0012). Each
/// controller type has its own UUID (ndeadly bluetooth_interface.md).
pub const CHR_RUMBLE_PRO: Uuid = uuid!("cc483f51-9258-427d-a939-630c31f72b05");
pub const CHR_RUMBLE_GC: Uuid = uuid!("3f8fb670-ab25-45bf-b540-38c72834d064");

pub fn rumble_uuid(pid: u16) -> Uuid {
    if pid == PID_GC { CHR_RUMBLE_GC } else { CHR_RUMBLE_PRO }
}
/// Command channel, write without response (handle 0x0014).
pub const CHR_CMD: Uuid = uuid!("649d4ac9-8eb7-4e6c-af44-1ea54fe5f005");
/// Command replies, notify (handle 0x001A).
pub const CHR_REPLY: Uuid = uuid!("c765a961-d9d8-4d36-a20a-5315b111836a");

/// Extracts the PID from manufacturer data as btleplug reports it (company ID
/// already stripped into the map key, so offsets are BTstack's minus two).
pub fn pid_from_mfr_data(data: &[u8]) -> Option<u16> {
    if data.len() < 7 {
        return None;
    }
    let vid = u16::from_le_bytes([data[3], data[4]]);
    let pid = u16::from_le_bytes([data[5], data[6]]);
    (vid == NINTENDO_VID).then_some(pid)
}

// --- Commands --------------------------------------------------------------
// Header: [cmd][0x91 request][0x01 BLE transport][subcmd][0x00][payload len][0x00][0x00] payload

pub const CMD_READ_SPI: u8 = 0x02;
pub const CMD_SET_LED: u8 = 0x09;
pub const CMD_FEATURE: u8 = 0x0C;
pub const CMD_PAIRING: u8 = 0x15;

fn command(cmd: u8, subcmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![cmd, 0x91, 0x01, subcmd, 0x00, payload.len() as u8, 0x00, 0x00];
    v.extend_from_slice(payload);
    v
}

/// One step of the init sequence: the bytes to send and the (cmd, subcmd)
/// that a reply must carry to advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitStep {
    ReadInfo,
    Pair1,
    Pair2,
    Pair3,
    Pair4,
    SetLed,
    Done,
}

impl InitStep {
    pub fn next(self) -> InitStep {
        use InitStep::*;
        match self {
            ReadInfo => Pair1,
            Pair1 => Pair2,
            Pair2 => Pair3,
            Pair3 => Pair4,
            Pair4 => SetLed,
            SetLed | Done => Done,
        }
    }

    /// Reply (cmd, subcmd) that acknowledges this step.
    pub fn expected_reply(self) -> Option<(u8, u8)> {
        use InitStep::*;
        match self {
            ReadInfo => Some((CMD_READ_SPI, 0x04)),
            Pair1 => Some((CMD_PAIRING, 0x01)),
            Pair2 => Some((CMD_PAIRING, 0x04)),
            Pair3 => Some((CMD_PAIRING, 0x02)),
            Pair4 => Some((CMD_PAIRING, 0x03)),
            SetLed => Some((CMD_SET_LED, 0x07)),
            Done => None,
        }
    }

    /// `host_addr` is the PC adapter address, most-significant byte first
    /// (as printed, e.g. AA:BB:CC:DD:EE:FF).
    pub fn bytes(self, host_addr: [u8; 6]) -> Option<Vec<u8>> {
        use InitStep::*;
        Some(match self {
            // Read 0x40 bytes of SPI flash at 0x00013000 (serial, VID/PID, colours).
            ReadInfo => command(CMD_READ_SPI, 0x04, &[0x40, 0x7e, 0x00, 0x00, 0x00, 0x30, 0x01, 0x00]),
            // Exchange addresses: two host addresses, reverse byte order, the
            // second with its low byte decremented.
            Pair1 => {
                let mut le = host_addr;
                le.reverse();
                let mut le2 = le;
                le2[0] = le2[0].wrapping_sub(1);
                let mut p = vec![0x00, 0x02];
                p.extend_from_slice(&le);
                p.extend_from_slice(&le2);
                command(CMD_PAIRING, 0x01, &p)
            }
            // Host key A1 (controller answers with fixed B1; LTK = A1 ^ B1).
            Pair2 => command(
                CMD_PAIRING,
                0x04,
                &[
                    0x00, 0xea, 0xbd, 0x47, 0x13, 0x89, 0x35, 0x42, 0xc6, 0x79, 0xee, 0x07, 0xf2, 0x53, 0x2c,
                    0x6c, 0x31,
                ],
            ),
            // LTK confirm challenge A2.
            Pair3 => command(
                CMD_PAIRING,
                0x02,
                &[
                    0x00, 0x40, 0xb0, 0x8a, 0x5f, 0xcd, 0x1f, 0x9b, 0x41, 0x12, 0x5c, 0xac, 0xc6, 0x3f, 0x38,
                    0xa0, 0x73,
                ],
            ),
            Pair4 => command(CMD_PAIRING, 0x03, &[0x00]),
            SetLed => set_led(0x01),
            Done => return None,
        })
    }
}

/// Player LED command; pattern 0x01/0x03/0x07/0x0F = players 1–4.
pub fn set_led(pattern: u8) -> Vec<u8> {
    command(CMD_SET_LED, 0x07, &[pattern, 0, 0, 0, 0, 0, 0, 0])
}

/// Feature enable (buttons|sticks|IMU|rumble = 0x27). Not sent by joypad-os
/// over BLE; used only as a fallback when no input arrives.
pub fn feature_cmds() -> [Vec<u8>; 2] {
    [
        command(CMD_FEATURE, 0x02, &[0x27, 0, 0, 0]),
        command(CMD_FEATURE, 0x04, &[0x27, 0, 0, 0]),
    ]
}

/// (cmd, subcmd) of a reply notification.
pub fn reply_id(reply: &[u8]) -> Option<(u8, u8)> {
    (reply.len() >= 4).then(|| (reply[0], reply[3]))
}

/// GameCube rumble packet: 21 bytes, [1] = 0x50 | tid, [2] = on/off.
pub fn gc_rumble(tid: u8, on: bool) -> [u8; 21] {
    let mut b = [0u8; 21];
    b[1] = 0x50 | (tid & 0x0F);
    b[2] = on as u8;
    b
}

// --- Input -----------------------------------------------------------------

pub mod btn {
    pub const Y: u32 = 1 << 0;
    pub const X: u32 = 1 << 1;
    pub const B: u32 = 1 << 2;
    pub const A: u32 = 1 << 3;
    pub const R: u32 = 1 << 6;
    pub const ZR: u32 = 1 << 7;
    pub const MINUS: u32 = 1 << 8;
    pub const PLUS: u32 = 1 << 9;
    pub const RSTICK: u32 = 1 << 10;
    pub const LSTICK: u32 = 1 << 11;
    pub const HOME: u32 = 1 << 12;
    pub const CAPTURE: u32 = 1 << 13;
    pub const C: u32 = 1 << 14;
    pub const DOWN: u32 = 1 << 16;
    pub const UP: u32 = 1 << 17;
    pub const RIGHT: u32 = 1 << 18;
    pub const LEFT: u32 = 1 << 19;
    pub const L: u32 = 1 << 22;
    pub const ZL: u32 = 1 << 23;
}

/// Raw decoded report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawReport {
    pub buttons: u32,
    /// 12-bit sticks: main (lx, ly), C-stick (rx, ry). Up is high.
    pub lx: u16,
    pub ly: u16,
    pub rx: u16,
    pub ry: u16,
    /// Analog triggers (GameCube only).
    pub lt: u8,
    pub rt: u8,
}

pub fn parse_report(data: &[u8]) -> Option<RawReport> {
    let r = if data.len() >= 64 && data[0] == 0xA1 { &data[1..] } else { data };
    if r.len() < 16 {
        return None;
    }
    let (lt, rt) = if r.len() >= 62 { (r[60], r[61]) } else { (0, 0) };
    Some(RawReport {
        buttons: u32::from_le_bytes([r[4], r[5], r[6], r[7]]),
        lx: r[10] as u16 | ((r[11] as u16 & 0x0F) << 8),
        ly: (r[11] as u16 >> 4) | ((r[12] as u16) << 4),
        rx: r[13] as u16 | ((r[14] as u16 & 0x0F) << 8),
        ry: (r[14] as u16 >> 4) | ((r[15] as u16) << 4),
        lt,
        rt,
    })
}

/// Normalised state: sticks in -1.0..=1.0 (up positive), triggers 0..=255.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct State {
    pub buttons: u32,
    pub lx: f32,
    pub ly: f32,
    pub rx: f32,
    pub ry: f32,
    pub lt: u8,
    pub rt: u8,
}

const CAL_SAMPLES: u8 = 4;
const GC_MAIN_RANGE: f32 = 1225.0;
const GC_CSTICK_RANGE: f32 = 1120.0;
const PRO_RANGE: f32 = 1610.0;
/// Fallback full-press value for GC triggers (joypad-os USB driver uses 20–240).
const TRIGGER_MAX_DEFAULT: u8 = 240;

/// Stick-centre and trigger-rest calibration taken from the first reports,
/// assuming the controller is untouched while connecting.
#[derive(Debug, Clone)]
pub struct Calibrator {
    samples: u8,
    center: [f32; 4],
    trig_min: [u8; 2],
    trig_max: [u8; 2],
    main_range: f32,
    c_range: f32,
}

impl Calibrator {
    pub fn new(pid: u16) -> Self {
        let gc = pid == PID_GC;
        Self {
            samples: 0,
            center: [2048.0; 4],
            trig_min: [0; 2],
            trig_max: [TRIGGER_MAX_DEFAULT; 2],
            main_range: if gc { GC_MAIN_RANGE } else { PRO_RANGE },
            c_range: if gc { GC_CSTICK_RANGE } else { PRO_RANGE },
        }
    }

    /// Returns `None` while still collecting calibration samples.
    pub fn apply(&mut self, r: &RawReport) -> Option<State> {
        let axes = [r.lx, r.ly, r.rx, r.ry].map(f32::from);
        if self.samples < CAL_SAMPLES {
            if self.samples == 0 {
                self.center = axes;
                self.trig_min = [r.lt, r.rt];
            } else {
                for i in 0..4 {
                    self.center[i] = (self.center[i] + axes[i]) / 2.0;
                }
                self.trig_min = [self.trig_min[0].max(r.lt), self.trig_min[1].max(r.rt)];
            }
            self.samples += 1;
            return None;
        }
        let norm = |v: f32, c: f32, range: f32| ((v - c) / range).clamp(-1.0, 1.0);
        let mut trig = |raw: u8, i: usize| -> u8 {
            self.trig_max[i] = self.trig_max[i].max(raw);
            let (lo, hi) = (self.trig_min[i] as f32, self.trig_max[i] as f32);
            if hi <= lo {
                return 0;
            }
            (((raw as f32 - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0).round() as u8
        };
        Some(State {
            buttons: r.buttons,
            lx: norm(axes[0], self.center[0], self.main_range),
            ly: norm(axes[1], self.center[1], self.main_range),
            rx: norm(axes[2], self.center[2], self.c_range),
            ry: norm(axes[3], self.center[3], self.c_range),
            lt: trig(r.lt, 0),
            rt: trig(r.rt, 1),
        })
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|x| u8::from_str_radix(x, 16).unwrap()).collect()
    }

    const ADDR: [u8; 6] = [0x48, 0xf1, 0xeb, 0x3a, 0xeb, 0x81];

    #[test]
    fn init_bytes_match_joypad_os() {
        let s = |st: InitStep| st.bytes(ADDR).unwrap();
        assert_eq!(s(InitStep::ReadInfo), h("02 91 01 04 00 08 00 00 40 7e 00 00 00 30 01 00"));
        // Matches the ndeadly commands.md example request for 48:F1:EB:3A:EB:81.
        assert_eq!(
            s(InitStep::Pair1),
            h("15 91 01 01 00 0e 00 00 00 02 81 eb 3a eb f1 48 80 eb 3a eb f1 48")
        );
        assert_eq!(
            s(InitStep::Pair2),
            h("15 91 01 04 00 11 00 00 00 ea bd 47 13 89 35 42 c6 79 ee 07 f2 53 2c 6c 31")
        );
        assert_eq!(
            s(InitStep::Pair3),
            h("15 91 01 02 00 11 00 00 00 40 b0 8a 5f cd 1f 9b 41 12 5c ac c6 3f 38 a0 73")
        );
        assert_eq!(s(InitStep::Pair4), h("15 91 01 03 00 01 00 00 00"));
        assert_eq!(s(InitStep::SetLed), h("09 91 01 07 00 08 00 00 01 00 00 00 00 00 00 00"));
        assert_eq!(InitStep::Done.bytes(ADDR), None);
    }

    #[test]
    fn mfr_data_pid() {
        // Pro Controller 2 advert from joypad-os, minus the 53 05 company ID.
        let d = h("01 00 03 7e 05 69 20 00 01 00 00 00 00 00 00 00 00 00 0f 00 00 00 00 00 00 00");
        assert_eq!(pid_from_mfr_data(&d), Some(PID_PRO2));
        assert_eq!(pid_from_mfr_data(&d[..5]), None);
    }

    #[test]
    fn parse_sticks_buttons_triggers() {
        let mut r = vec![0u8; 63];
        r[4..8].copy_from_slice(&(btn::A | btn::ZR | btn::UP | btn::L).to_le_bytes());
        // lx=0x123 ly=0x456 rx=0x789 ry=0xabc
        r[10] = 0x23;
        r[11] = 0x61;
        r[12] = 0x45;
        r[13] = 0x89;
        r[14] = 0xc7;
        r[15] = 0xab;
        r[60] = 30;
        r[61] = 200;
        let p = parse_report(&r).unwrap();
        assert_eq!((p.lx, p.ly, p.rx, p.ry), (0x123, 0x456, 0x789, 0xabc));
        assert_eq!(p.buttons, btn::A | btn::ZR | btn::UP | btn::L);
        assert_eq!((p.lt, p.rt), (30, 200));

        let mut a1 = vec![0xA1];
        a1.extend_from_slice(&r);
        assert_eq!(parse_report(&a1), Some(p));
        assert_eq!(parse_report(&r[..10]), None);
    }

    #[test]
    fn calibration() {
        let rest = RawReport { lx: 2000, ly: 2100, rx: 1990, ry: 2050, lt: 25, rt: 30, ..Default::default() };
        let mut c = Calibrator::new(PID_GC);
        for _ in 0..4 {
            assert!(c.apply(&rest).is_none());
        }
        let s = c.apply(&rest).unwrap();
        assert_eq!((s.lx, s.ly, s.rx, s.ry, s.lt, s.rt), (0.0, 0.0, 0.0, 0.0, 0, 0));
        let s = c.apply(&RawReport { lx: 2000 + 1225, ly: 0, lt: 240, ..rest }).unwrap();
        assert_eq!((s.lx, s.ly, s.lt), (1.0, -1.0, 255));
    }

    #[test]
    fn rumble_packet() {
        let p = gc_rumble(0x13, true);
        assert_eq!(p.len(), 21);
        assert_eq!(&p[..3], &[0x00, 0x53, 0x01]);
        assert!(p[3..].iter().all(|&b| b == 0));
        assert_eq!(gc_rumble(0, false)[2], 0);
    }
}
