//! BLE central: find, connect, run the Switch 2 init/pairing handshake, and
//! stream input reports.

use crate::protocol::{self, InitStep, hex};
#[cfg(windows)]
use crate::winconn::FastLink;
#[cfg_attr(target_os = "linux", allow(unused_imports))]
use anyhow::{Context, Result, anyhow, bail};
#[cfg(not(target_os = "linux"))]
use btleplug::api::{CharPropFlags, Characteristic, WriteType};
use btleplug::api::{BDAddr, Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use log::{debug, info, warn};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::time::{interval, timeout};

const STEP_RETRY: Duration = Duration::from_millis(500);
const RUMBLE_REFRESH: Duration = Duration::from_millis(50);
const NO_INPUT_FALLBACK: Duration = Duration::from_secs(2);

pub async fn adapter() -> Result<Adapter> {
    let manager = Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no Bluetooth adapter found"))
}

pub async fn host_address(adapter: &Adapter) -> Result<[u8; 6]> {
    let addr: BDAddr = adapter
        .adapter_address()
        .await?
        .ok_or_else(|| anyhow!("could not read the PC's Bluetooth address"))?;
    Ok(addr.into_inner())
}

/// Scans until a Switch 2 controller advertises. With `print_all`, every
/// Nintendo advert is logged and scanning continues until `wait` elapses.
pub type Events = std::pin::Pin<Box<dyn futures::Stream<Item = CentralEvent> + Send>>;

pub async fn events(adapter: &Adapter) -> Result<Events> {
    Ok(adapter.events().await?)
}

pub async fn find_controller(
    adapter: &Adapter,
    events: &mut Events,
    want: &[u16],
    wait: Option<Duration>,
    print_all: bool,
) -> Result<Option<(Peripheral, u16)>> {
    // Skip events queued while we were connected.
    while let Some(Some(_)) = futures::FutureExt::now_or_never(events.next()) {}
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = wait.map(|w| Instant::now() + w);
    let mut seen = std::collections::HashSet::new();
    let found = loop {
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if left == Some(Duration::ZERO) {
            break None;
        }
        let ev = match left {
            Some(l) => match timeout(l, events.next()).await {
                Ok(ev) => ev,
                Err(_) => break None,
            },
            None => events.next().await,
        };
        let Some(ev) = ev else { break None };
        let (id, mfr) = match ev {
            CentralEvent::ManufacturerDataAdvertisement { id, manufacturer_data } => (id, Some(manufacturer_data)),
            CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => (id, None),
            CentralEvent::RssiUpdate { id, .. } => (id, None),
            _ => continue,
        };
        let Ok(p) = adapter.peripheral(&id).await else { continue };
        let mfr = match mfr {
            Some(m) => m,
            None => match p.properties().await {
                Ok(Some(props)) => props.manufacturer_data,
                _ => continue,
            },
        };
        let Some(data) = mfr.get(&protocol::NINTENDO_COMPANY_ID) else { continue };
        let Some(pid) = protocol::pid_from_mfr_data(data) else { continue };
        if print_all {
            let addr = p.address();
            if !seen.insert((addr, data.clone())) {
                continue;
            }
            println!("{addr}  PID {pid:#06x} {:<24} mfr: {}", protocol::pid_name(pid), hex(data));
            continue;
        }
        if want.contains(&pid) {
            info!("found {} ({pid:#06x}) at {}", protocol::pid_name(pid), p.address());
            break Some((p, pid));
        }
    };
    adapter.stop_scan().await.ok();
    Ok(found)
}

pub struct Session {
    pub pid: u16,
    t: Transport,
    #[cfg(windows)]
    link: Option<FastLink>,
}

/// A notification from the controller.
enum Note {
    Input(Vec<u8>),
    Reply(Vec<u8>),
    Other(String, Vec<u8>),
}

#[cfg(not(target_os = "linux"))]
struct Transport {
    p: Peripheral,
    cmd: Characteristic,
    rumble: Option<Characteristic>,
    notifs: tokio::sync::Mutex<std::pin::Pin<Box<dyn futures::Stream<Item = btleplug::api::ValueNotification> + Send>>>,
}

#[cfg(target_os = "linux")]
struct Transport {
    att: crate::att::AttLink,
    events: tokio::sync::Mutex<crate::att::AttEvents>,
    cmd: u16,
    reply: u16,
    input: u16,
    rumble: Option<u16>,
}

#[cfg(not(target_os = "linux"))]
fn find_char(p: &Peripheral, uuid: uuid::Uuid) -> Option<Characteristic> {
    p.characteristics().into_iter().find(|c| c.uuid == uuid)
}

#[cfg(not(target_os = "linux"))]
fn write_type(c: &Characteristic) -> WriteType {
    if c.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
        WriteType::WithoutResponse
    } else {
        WriteType::WithResponse
    }
}

#[cfg(not(target_os = "linux"))]
impl Transport {
    async fn open(p: Peripheral, pid: u16) -> Result<Self> {
        p.connect_with_timeout(Duration::from_secs(10)).await.context("connect")?;
        p.discover_services().await.context("discover services")?;
        let cmd = find_char(&p, protocol::CHR_CMD).ok_or_else(|| anyhow!("command characteristic not found"))?;
        let reply = find_char(&p, protocol::CHR_REPLY).ok_or_else(|| anyhow!("reply characteristic not found"))?;
        let input = find_char(&p, protocol::CHR_INPUT).ok_or_else(|| anyhow!("input characteristic not found"))?;
        let rumble = find_char(&p, protocol::rumble_uuid(pid));
        // Same order as joypad-os: replies first, then input.
        p.subscribe(&reply).await.context("subscribe to replies")?;
        p.subscribe(&input).await.context("subscribe to input")?;
        let notifs = tokio::sync::Mutex::new(p.notifications().await?);
        Ok(Self { p, cmd, rumble, notifs })
    }

    fn print_gatt(&self) {
        for s in self.p.services() {
            println!("service {}", s.uuid);
            for c in &s.characteristics {
                println!("  char {}  {:?}", c.uuid, c.properties);
            }
        }
    }

    async fn send_cmd(&self, b: &[u8]) -> Result<()> {
        self.p.write(&self.cmd, b, write_type(&self.cmd)).await.context("write command")
    }

    async fn send_rumble(&self, b: &[u8]) -> Result<()> {
        match &self.rumble {
            Some(c) => self.p.write(c, b, write_type(c)).await.context("write rumble"),
            None => Ok(()),
        }
    }

    async fn next(&self) -> Option<Note> {
        let n = self.notifs.lock().await.next().await?;
        Some(if n.uuid == protocol::CHR_INPUT {
            Note::Input(n.value)
        } else if n.uuid == protocol::CHR_REPLY {
            Note::Reply(n.value)
        } else {
            Note::Other(n.uuid.to_string(), n.value)
        })
    }

    async fn alive(&self) -> bool {
        self.p.is_connected().await.unwrap_or(false)
    }

    async fn close(&self) {
        self.p.disconnect().await.ok();
    }
}

#[cfg(target_os = "linux")]
impl Transport {
    async fn open(p: Peripheral, pid: u16) -> Result<Self> {
        use crate::att::AttLink;
        let (att, mut events) = timeout(Duration::from_secs(15), AttLink::connect(p.address().into_inner()))
            .await
            .map_err(|_| anyhow!("connect timed out"))??;
        let h = |u| att.handle(u).ok_or_else(|| anyhow!("characteristic {u} not found"));
        let (cmd, reply, input) = (h(protocol::CHR_CMD)?, h(protocol::CHR_REPLY)?, h(protocol::CHR_INPUT)?);
        let rumble = att.handle(protocol::rumble_uuid(pid));
        // Same order as joypad-os: replies first, then input.
        att.subscribe(&mut events, reply).await?;
        att.subscribe(&mut events, input).await?;
        if let Err(e) = att.request_fast_interval() {
            warn!("could not request a faster connection interval: {e:#}");
        }
        Ok(Self { att, events: tokio::sync::Mutex::new(events), cmd, reply, input, rumble })
    }

    fn print_gatt(&self) {
        for c in &self.att.chars {
            println!("  char {:#06x} {}  props {:#04x}", c.handle, c.uuid, c.props);
        }
    }

    async fn send_cmd(&self, b: &[u8]) -> Result<()> {
        self.att.write_cmd(self.cmd, b)
    }

    async fn send_rumble(&self, b: &[u8]) -> Result<()> {
        match self.rumble {
            Some(h) => self.att.write_cmd(h, b),
            None => Ok(()),
        }
    }

    async fn next(&self) -> Option<Note> {
        let (h, v) = self.events.lock().await.next().await?;
        Some(if h == self.input {
            Note::Input(v)
        } else if h == self.reply {
            Note::Reply(v)
        } else {
            Note::Other(format!("{h:#06x}"), v)
        })
    }

    async fn alive(&self) -> bool {
        true // a dropped link ends the notification stream
    }

    async fn close(&self) {
        self.att.close();
    }
}

impl Session {
    pub async fn connect(p: Peripheral, pid: u16) -> Result<Self> {
        info!("connecting…");
        #[cfg(windows)]
        let addr = p.address().into_inner();
        let t = Transport::open(p, pid).await?;
        #[cfg(windows)]
        let link = match FastLink::request(addr) {
            Ok(l) => Some(l),
            Err(e) => {
                warn!("could not request a faster connection interval: {e:#}");
                None
            }
        };
        info!("connected and subscribed");
        Ok(Self {
            pid,
            t,
            #[cfg(windows)]
            link,
        })
    }

    pub fn print_gatt(&self) {
        self.t.print_gatt();
    }

    async fn send(&self, bytes: &[u8]) -> Result<()> {
        debug!("cmd → {}", hex(bytes));
        self.t.send_cmd(bytes).await
    }

    /// Runs the handshake and streams input until the controller disconnects.
    /// `on_input` gets every raw input notification. `rumble` carries the
    /// requested strength (0 = off).
    pub async fn run(
        &self,
        host_addr: [u8; 6],
        mut on_input: impl FnMut(&[u8]),
        mut rumble: Option<watch::Receiver<u8>>,
    ) -> Result<()> {
        let mut step = InitStep::ReadInfo;
        let mut step_sent = Instant::now();
        self.send(&step.bytes(host_addr).unwrap()).await?;

        let mut retry = interval(STEP_RETRY);
        let mut rumble_tick = interval(RUMBLE_REFRESH);
        let mut health = interval(Duration::from_secs(1));
        let mut last_input: Option<Instant> = None;
        let mut done_at: Option<Instant> = None;
        let mut feature_sent = false;
        let mut short_warned = false;
        let (mut tid, mut rumble_on) = (0u8, false);
        let (mut reports, mut rate_since) = (0u32, Instant::now());

        loop {
            tokio::select! {
                n = self.t.next() => match n {
                    None => bail!("controller disconnected"),
                    Some(Note::Input(v)) => {
                        if v.len() < 62 && !short_warned {
                            warn!("input report only {} bytes (MTU too small?) — triggers unavailable", v.len());
                            short_warned = true;
                        }
                        last_input = Some(Instant::now());
                        reports += 1;
                        on_input(&v);
                    }
                    Some(Note::Reply(v)) => {
                        debug!("reply ← {}", hex(&v));
                        if protocol::reply_id(&v) == step.expected_reply() {
                            if step == InitStep::Pair4 {
                                info!("pairing handshake complete");
                            }
                            step = step.next();
                            match step.bytes(host_addr) {
                                Some(b) => { self.send(&b).await?; step_sent = Instant::now(); }
                                None => { info!("init done"); done_at = Some(Instant::now()); }
                            }
                        }
                    }
                    Some(Note::Other(src, v)) => debug!("notify {src} ← {}", hex(&v)),
                },
                _ = retry.tick() => {
                    if step != InitStep::Done && step_sent.elapsed() >= STEP_RETRY {
                        debug!("retrying {step:?}");
                        self.send(&step.bytes(host_addr).unwrap()).await?;
                        step_sent = Instant::now();
                    }
                }
                _ = rumble_tick.tick(), if rumble.is_some() && self.pid == protocol::PID_GC => {
                    let want = *rumble.as_mut().unwrap().borrow_and_update() > 0;
                    // Refresh while on; send a single "off" on release.
                    if want != rumble_on {
                        info!("rumble {}", if want { "on" } else { "off" });
                    }
                    if want || rumble_on {
                        let pkt = protocol::gc_rumble(tid, want);
                        tid = tid.wrapping_add(1);
                        if let Err(e) = self.t.send_rumble(&pkt).await {
                            debug!("rumble write failed: {e}");
                        }
                        rumble_on = want;
                    }
                }
                _ = health.tick() => {
                    if !self.t.alive().await {
                        bail!("controller disconnected");
                    }
                    if rate_since.elapsed() >= Duration::from_secs(5) {
                        let hz = reports as f32 / rate_since.elapsed().as_secs_f32();
                        #[cfg(windows)]
                        let iv = self.link.as_ref().and_then(|l| l.interval_ms());
                        #[cfg(not(windows))]
                        let iv: Option<f32> = None;
                        info!("input {hz:.1} Hz, connection interval {}", iv.map_or("?".into(), |v| format!("{v:.2} ms")));
                        (reports, rate_since) = (0, Instant::now());
                    }
                    if let Some(t) = done_at
                        && !feature_sent
                        && last_input.is_none()
                        && t.elapsed() >= NO_INPUT_FALLBACK
                    {
                        warn!("no input yet; sending feature-enable commands");
                        for c in protocol::feature_cmds() {
                            self.send(&c).await?;
                        }
                        feature_sent = true;
                    }
                }
            }
        }
    }

    pub async fn disconnect(&self) {
        self.t.close().await;
    }
}
