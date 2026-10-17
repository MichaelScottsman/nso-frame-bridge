//! Virtual Xbox 360 controller: ViGEmBus on Windows, uinput on Linux.

use crate::protocol::{State, btn};

#[cfg(windows)]
pub use self::windows::Pad;
#[cfg(target_os = "linux")]
pub use self::linux::Pad;

/// XInput button bits (shared by both backends).
pub mod xb {
    pub const UP: u16 = 0x0001;
    pub const DOWN: u16 = 0x0002;
    pub const LEFT: u16 = 0x0004;
    pub const RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LTHUMB: u16 = 0x0040;
    pub const RTHUMB: u16 = 0x0080;
    pub const LB: u16 = 0x0100;
    pub const RB: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// Xbox 360 pad state. Sticks are XInput-style: up is positive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XState {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

/// GameCube → Xbox 360 mapping (by label: A→A, B→B, X→X, Y→Y).
pub fn to_xinput(s: &State) -> XState {
    const MAP: &[(u32, u16)] = &[
        (btn::A, xb::A),
        (btn::B, xb::B),
        (btn::X, xb::X),
        (btn::Y, xb::Y),
        (btn::ZR, xb::RB), // GC "Z"
        (btn::ZL, xb::LB),
        (btn::PLUS, xb::START),
        (btn::MINUS, xb::BACK),
        (btn::CAPTURE, xb::BACK),
        (btn::C, xb::BACK),
        (btn::HOME, xb::GUIDE),
        (btn::LSTICK, xb::LTHUMB),
        (btn::RSTICK, xb::RTHUMB),
        (btn::UP, xb::UP),
        (btn::DOWN, xb::DOWN),
        (btn::LEFT, xb::LEFT),
        (btn::RIGHT, xb::RIGHT),
    ];
    let buttons = MAP.iter().filter(|(b, _)| s.buttons & b != 0).fold(0u16, |acc, (_, x)| acc | x);
    let axis = |v: f32| (v * 32767.0).round() as i16;
    // The digital L/R click (full press) forces the trigger to max.
    let trig = |analog: u8, click: u32| if s.buttons & click != 0 { 255 } else { analog };
    XState {
        buttons,
        lt: trig(s.lt, btn::L),
        rt: trig(s.rt, btn::R),
        lx: axis(s.lx),
        ly: axis(s.ly), // XInput up is positive, same as Nintendo
        rx: axis(s.rx),
        ry: axis(s.ry),
    }
}

#[cfg(windows)]
mod windows {
    use super::{XState, to_xinput};
    use crate::protocol::State;
    use anyhow::{Context, Result};
    use tokio::sync::watch;
    use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};

    pub struct Pad {
        target: Xbox360Wired<Client>,
    }

    impl Pad {
        /// Plugs in a virtual pad. `rumble` receives the max of the two motor
        /// strengths (0 = off) whenever a game changes vibration.
        pub fn new(rumble: watch::Sender<u8>) -> Result<Self> {
            let client = Client::connect().context("connect to ViGEmBus (is the driver installed?)")?;
            let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
            target.plugin().context("plug in virtual Xbox 360 pad")?;
            target.wait_ready().context("wait for virtual pad")?;
            target
                .request_notification()
                .context("request rumble notifications")?
                .spawn_thread(move |_, n| {
                    let _ = rumble.send(n.large_motor.max(n.small_motor));
                });
            Ok(Self { target })
        }

        pub fn update(&mut self, s: &State) -> Result<()> {
            self.send(&to_xinput(s))
        }

        pub fn neutral(&mut self) -> Result<()> {
            self.send(&XState::default())
        }

        fn send(&mut self, x: &XState) -> Result<()> {
            let g = XGamepad {
                buttons: XButtons { raw: x.buttons },
                left_trigger: x.lt,
                right_trigger: x.rt,
                thumb_lx: x.lx,
                thumb_ly: x.ly,
                thumb_rx: x.rx,
                thumb_ry: x.ry,
            };
            self.target.update(&g).context("update virtual pad")
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! uinput device that mimics the kernel xpad driver's Xbox 360 pad
    //! (045e:028e), so Steam/SDL treat it as a normal Xbox 360 controller.
    use super::{XState, to_xinput, xb};
    use crate::protocol::State;
    use anyhow::{Context, Result};
    use evdev::{
        AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
        FFEffectKind, InputEvent, InputId, KeyCode, UInputCode, UinputAbsSetup, uinput::VirtualDevice,
    };
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tokio::sync::watch;

    const KEYS: &[(u16, KeyCode)] = &[
        (xb::A, KeyCode::BTN_SOUTH),
        (xb::B, KeyCode::BTN_EAST),
        (xb::X, KeyCode::BTN_NORTH),
        (xb::Y, KeyCode::BTN_WEST),
        (xb::LB, KeyCode::BTN_TL),
        (xb::RB, KeyCode::BTN_TR),
        (xb::BACK, KeyCode::BTN_SELECT),
        (xb::START, KeyCode::BTN_START),
        (xb::GUIDE, KeyCode::BTN_MODE),
        (xb::LTHUMB, KeyCode::BTN_THUMBL),
        (xb::RTHUMB, KeyCode::BTN_THUMBR),
    ];

    pub struct Pad {
        tx: mpsc::Sender<XState>,
    }

    impl Pad {
        pub fn new(rumble: watch::Sender<u8>) -> Result<Self> {
            let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
            let trig = AbsInfo::new(0, 0, 255, 0, 0, 0);
            let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
            let mut b = VirtualDevice::builder()
                .context("open /dev/uinput")?
                .name("Microsoft X-Box 360 pad")
                .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114))
                .with_keys(&AttributeSet::from_iter(KEYS.iter().map(|(_, k)| *k)))?;
            for (code, info) in [
                (Abs::ABS_X, stick),
                (Abs::ABS_Y, stick),
                (Abs::ABS_RX, stick),
                (Abs::ABS_RY, stick),
                (Abs::ABS_Z, trig),
                (Abs::ABS_RZ, trig),
                (Abs::ABS_HAT0X, hat),
                (Abs::ABS_HAT0Y, hat),
            ] {
                b = b.with_absolute_axis(&UinputAbsSetup::new(code, info))?;
            }
            let mut dev = b
                .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))?
                .with_ff_effects_max(16)
                .build()
                .context("create uinput device (is /dev/uinput writable?)")?;
            // FF requests are polled between input updates on one thread.
            let fd = dev.as_raw_fd();
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
            let (tx, rx) = mpsc::channel::<XState>();
            std::thread::spawn(move || device_thread(&mut dev, rx, rumble));
            Ok(Self { tx })
        }

        pub fn update(&mut self, s: &State) -> Result<()> {
            self.tx.send(to_xinput(s)).context("uinput thread stopped")
        }

        pub fn neutral(&mut self) -> Result<()> {
            self.tx.send(XState::default()).context("uinput thread stopped")
        }
    }

    fn events(x: &XState) -> Vec<InputEvent> {
        let abs = |c: Abs, v: i32| InputEvent::new(EventType::ABSOLUTE.0, c.0, v);
        let dir = |neg: u16, pos: u16| (x.buttons & pos != 0) as i32 - (x.buttons & neg != 0) as i32;
        let mut ev: Vec<InputEvent> = KEYS
            .iter()
            .map(|(bit, k)| InputEvent::new(EventType::KEY.0, k.0, (x.buttons & bit != 0) as i32))
            .collect();
        ev.extend([
            abs(Abs::ABS_X, x.lx as i32),
            abs(Abs::ABS_Y, -(x.ly as i32)), // evdev: down is positive
            abs(Abs::ABS_RX, x.rx as i32),
            abs(Abs::ABS_RY, -(x.ry as i32)),
            abs(Abs::ABS_Z, x.lt as i32),
            abs(Abs::ABS_RZ, x.rt as i32),
            abs(Abs::ABS_HAT0X, dir(xb::LEFT, xb::RIGHT)),
            abs(Abs::ABS_HAT0Y, dir(xb::UP, xb::DOWN)),
        ]);
        ev
    }

    struct Effect {
        level: u8,
        length: Duration,
    }

    fn device_thread(dev: &mut VirtualDevice, rx: mpsc::Receiver<XState>, rumble: watch::Sender<u8>) {
        let mut effects: HashMap<i16, Effect> = HashMap::new();
        let mut playing: HashMap<i16, Option<Instant>> = HashMap::new();
        let mut next_id: i16 = 0;
        let mut last_level = 0u8;
        loop {
            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(mut x) => {
                    // Only the newest state matters.
                    while let Ok(newer) = rx.try_recv() {
                        x = newer;
                    }
                    if let Err(e) = dev.emit(&events(&x)) {
                        log::error!("uinput emit: {e}");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }

            let pending: Vec<InputEvent> = match dev.fetch_events() {
                Ok(it) => it.collect(),
                Err(_) => Vec::new(), // EAGAIN
            };
            for ev in pending {
                match ev.destructure() {
                    EventSummary::UInput(ev, UInputCode::UI_FF_UPLOAD, ..) => {
                        if let Ok(mut up) = dev.process_ff_upload(ev) {
                            let data = up.effect();
                            let mut id = up.effect_id();
                            if id < 0 {
                                id = next_id;
                                next_id = (next_id + 1) % 16;
                                up.set_effect_id(id);
                            }
                            let level = match data.kind {
                                FFEffectKind::Rumble { strong_magnitude, weak_magnitude } => {
                                    (strong_magnitude.max(weak_magnitude) >> 8) as u8
                                }
                                _ => 0,
                            };
                            let length = Duration::from_millis(data.replay.length as u64);
                            effects.insert(id, Effect { level, length });
                            up.set_retval(0);
                        }
                    }
                    EventSummary::UInput(ev, UInputCode::UI_FF_ERASE, ..) => {
                        if let Ok(er) = dev.process_ff_erase(ev) {
                            effects.remove(&(er.effect_id() as i16));
                            playing.remove(&(er.effect_id() as i16));
                        }
                    }
                    EventSummary::ForceFeedback(_, code, value) => {
                        let id = code.0 as i16;
                        if value > 0 {
                            let until = effects
                                .get(&id)
                                .filter(|e| !e.length.is_zero())
                                .map(|e| Instant::now() + e.length);
                            playing.insert(id, until);
                        } else {
                            playing.remove(&id);
                        }
                    }
                    _ => {}
                }
            }

            let now = Instant::now();
            playing.retain(|_, until| until.is_none_or(|t| t > now));
            let level = playing.keys().filter_map(|id| effects.get(id)).map(|e| e.level).max().unwrap_or(0);
            if level != last_level {
                last_level = level;
                let _ = rumble.send(level);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        let s = State { buttons: btn::A | btn::ZR | btn::R, lx: 1.0, ly: -1.0, lt: 100, rt: 10, ..Default::default() };
        let x = to_xinput(&s);
        assert_eq!(x.buttons, xb::A | xb::RB);
        assert_eq!((x.lx, x.ly), (32767, -32767));
        assert_eq!((x.lt, x.rt), (100, 255));
    }
}
