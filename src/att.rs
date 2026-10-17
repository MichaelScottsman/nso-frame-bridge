//! Linux: raw ATT over an L2CAP LE socket, bypassing BlueZ's GATT client.
//!
//! BlueZ discovers the full GATT table on connect, including secondary
//! services. Nintendo's firmware never answers that request, so the bearer
//! stalls until the link times out. Opening our own ATT client socket first
//! makes the kernel hand the ATT channel to us instead of bluetoothd
//! (`l2cap_le_conn_ready`: "Client ATT sockets should override the server
//! one"), and we discover just the characteristics we need.

use crate::protocol;
use anyhow::{Context, Result, anyhow, bail};
use log::{debug, info};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_L2CAP: i32 = 0;
const SOL_BLUETOOTH: i32 = 274;
const BT_SECURITY: i32 = 4;
const BT_SECURITY_LOW: u8 = 1;
const ATT_CID: u16 = 4;
const BDADDR_LE_PUBLIC: u8 = 1;
const OUR_MTU: u16 = 517;
const CONNECT_TIMEOUT_MS: i32 = 10_000;

// ATT opcodes
const ERROR_RSP: u8 = 0x01;
const MTU_REQ: u8 = 0x02;
const MTU_RSP: u8 = 0x03;
const READ_BY_TYPE_REQ: u8 = 0x08;
const READ_BY_TYPE_RSP: u8 = 0x09;
const WRITE_REQ: u8 = 0x12;
const WRITE_RSP: u8 = 0x13;
const NOTIFY: u8 = 0x1B;
const INDICATE: u8 = 0x1D;
const CONFIRM: u8 = 0x1E;
const WRITE_CMD: u8 = 0x52;

#[repr(C)]
struct SockaddrL2 {
    family: u16,
    psm: u16,
    bdaddr: [u8; 6],
    cid: u16,
    bdaddr_type: u8,
}

#[derive(Debug, Clone)]
pub struct Char {
    pub uuid: Uuid,
    pub props: u8,
    pub handle: u16,
}

/// Write side plus the discovered characteristic table.
pub struct AttLink {
    fd: Arc<OwnedFd>,
    pub chars: Vec<Char>,
    pub mtu: u16,
}

/// Read side: responses, notifications and indications from the controller.
pub struct AttEvents {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

fn sockaddr(addr_msb_first: [u8; 6]) -> SockaddrL2 {
    let mut le = addr_msb_first;
    le.reverse();
    SockaddrL2 { family: AF_BLUETOOTH as u16, psm: 0, bdaddr: le, cid: ATT_CID.to_le(), bdaddr_type: BDADDR_LE_PUBLIC }
}

fn send_raw(fd: &OwnedFd, pdu: &[u8]) -> Result<()> {
    let n = unsafe { libc::send(fd.as_raw_fd(), pdu.as_ptr().cast(), pdu.len(), 0) };
    if n < 0 {
        bail!("ATT send: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Opcodes that are requests from the peer and need an answer.
fn is_request(op: u8) -> bool {
    matches!(op, 0x02 | 0x04 | 0x06 | 0x08 | 0x0A | 0x0C | 0x0E | 0x10 | 0x12 | 0x16 | 0x18 | 0x20)
}

impl AttLink {
    pub async fn connect(addr: [u8; 6]) -> Result<(Self, AttEvents)> {
        let fd = tokio::task::spawn_blocking(move || -> Result<OwnedFd> {
            let raw = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, BTPROTO_L2CAP) };
            if raw < 0 {
                bail!("socket: {}", std::io::Error::last_os_error());
            }
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            let local = SockaddrL2 { bdaddr: [0; 6], ..sockaddr([0; 6]) };
            let len = std::mem::size_of::<SockaddrL2>() as u32;
            if unsafe { libc::bind(raw, (&local as *const SockaddrL2).cast(), len) } < 0 {
                bail!("bind: {}", std::io::Error::last_os_error());
            }
            let sec = [BT_SECURITY_LOW, 0u8];
            unsafe { libc::setsockopt(raw, SOL_BLUETOOTH, BT_SECURITY, sec.as_ptr().cast(), 2) };
            let peer = sockaddr(addr);
            unsafe {
                let fl = libc::fcntl(raw, libc::F_GETFL);
                libc::fcntl(raw, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
            if unsafe { libc::connect(raw, (&peer as *const SockaddrL2).cast(), len) } < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EINPROGRESS) {
                    bail!("L2CAP connect: {e}");
                }
                let mut pfd = libc::pollfd { fd: raw, events: libc::POLLOUT, revents: 0 };
                let n = unsafe { libc::poll(&mut pfd, 1, CONNECT_TIMEOUT_MS) };
                if n == 0 {
                    bail!("L2CAP connect timed out"); // dropping fd cancels it
                }
                let mut err: i32 = 0;
                let mut elen = std::mem::size_of::<i32>() as u32;
                unsafe {
                    libc::getsockopt(raw, libc::SOL_SOCKET, libc::SO_ERROR, (&mut err as *mut i32).cast(), &mut elen)
                };
                if n < 0 || err != 0 {
                    bail!("L2CAP connect: {}", std::io::Error::from_raw_os_error(if n < 0 { libc::EIO } else { err }));
                }
            }
            unsafe {
                let fl = libc::fcntl(raw, libc::F_GETFL);
                libc::fcntl(raw, libc::F_SETFL, fl & !libc::O_NONBLOCK);
            }
            Ok(fd)
        })
        .await??;
        let fd = Arc::new(fd);

        // Reader thread: answers peer requests (we expose no GATT server) and
        // forwards everything else.
        let (tx, rx) = mpsc::unbounded_channel();
        let rfd = fd.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1024];
            loop {
                let n = unsafe { libc::recv(rfd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
                if n <= 0 {
                    return; // closed: dropping tx ends the stream
                }
                let pdu = &buf[..n as usize];
                match pdu[0] {
                    MTU_REQ => {
                        let _ = send_raw(&rfd, &[MTU_RSP, OUR_MTU as u8, (OUR_MTU >> 8) as u8]);
                    }
                    INDICATE => {
                        let _ = send_raw(&rfd, &[CONFIRM]);
                        if tx.send(pdu.to_vec()).is_err() {
                            return;
                        }
                    }
                    op if is_request(op) => {
                        // "Attribute not found" for anything aimed at our (empty) server.
                        let _ = send_raw(&rfd, &[ERROR_RSP, op, 0x01, 0x00, 0x0A]);
                    }
                    _ => {
                        if tx.send(pdu.to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });

        let mut link = AttLink { fd, chars: Vec::new(), mtu: 23 };
        let mut events = AttEvents { rx };
        link.setup(&mut events).await?;
        Ok((link, events))
    }

    /// Sends a request and waits for its response, dropping notifications.
    async fn request(&self, ev: &mut AttEvents, pdu: &[u8], rsp: u8) -> Result<Vec<u8>> {
        send_raw(&self.fd, pdu)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let r = tokio::time::timeout_at(deadline, ev.rx.recv())
                .await
                .map_err(|_| anyhow!("ATT request {:#04x} timed out", pdu[0]))?
                .ok_or_else(|| anyhow!("disconnected"))?;
            if r[0] == rsp || (r[0] == ERROR_RSP && r.get(1) == Some(&pdu[0])) {
                return Ok(r);
            }
        }
    }

    async fn setup(&mut self, ev: &mut AttEvents) -> Result<()> {
        let r = self.request(ev, &[MTU_REQ, OUR_MTU as u8, (OUR_MTU >> 8) as u8], MTU_RSP).await?;
        if r[0] == MTU_RSP && r.len() >= 3 {
            self.mtu = OUR_MTU.min(u16::from_le_bytes([r[1], r[2]]));
        }
        debug!("ATT MTU {}", self.mtu);

        // Characteristic declarations (0x2803) across the whole table.
        let mut start = 1u16;
        while start != 0 {
            let r = self
                .request(ev, &[READ_BY_TYPE_REQ, start as u8, (start >> 8) as u8, 0xff, 0xff, 0x03, 0x28], READ_BY_TYPE_RSP)
                .await?;
            if r[0] != READ_BY_TYPE_RSP || r.len() < 2 {
                break;
            }
            let len = r[1] as usize;
            if len < 7 {
                break;
            }
            let mut last = start;
            for e in r[2..].chunks_exact(len) {
                let decl = u16::from_le_bytes([e[0], e[1]]);
                let handle = u16::from_le_bytes([e[3], e[4]]);
                let uuid = match &e[5..] {
                    u if u.len() == 16 => {
                        let mut b: [u8; 16] = u.try_into().unwrap();
                        b.reverse();
                        Uuid::from_bytes(b)
                    }
                    u if u.len() == 2 => Uuid::from_u128(
                        ((u16::from_le_bytes([u[0], u[1]]) as u128) << 96) | 0x0000_0000_0000_1000_8000_00805f9b34fb,
                    ),
                    _ => continue,
                };
                self.chars.push(Char { uuid, props: e[2], handle });
                last = decl;
            }
            start = last.checked_add(1).unwrap_or(0);
        }
        info!("discovered {} characteristics (ATT MTU {})", self.chars.len(), self.mtu);
        Ok(())
    }

    pub fn handle(&self, uuid: Uuid) -> Option<u16> {
        self.chars.iter().find(|c| c.uuid == uuid).map(|c| c.handle)
    }

    /// Enables notifications (CCC is assumed to follow the value handle, as
    /// on all Switch 2 controllers).
    pub async fn subscribe(&self, ev: &mut AttEvents, value_handle: u16) -> Result<()> {
        let ccc = value_handle + 1;
        let r = self.request(ev, &[WRITE_REQ, ccc as u8, (ccc >> 8) as u8, 0x01, 0x00], WRITE_RSP).await?;
        if r[0] != WRITE_RSP {
            bail!("enabling notifications on {value_handle:#06x} failed: {}", protocol::hex(&r));
        }
        Ok(())
    }

    pub fn write_cmd(&self, handle: u16, value: &[u8]) -> Result<()> {
        let mut pdu = Vec::with_capacity(value.len() + 3);
        pdu.extend_from_slice(&[WRITE_CMD, handle as u8, (handle >> 8) as u8]);
        pdu.extend_from_slice(value);
        send_raw(&self.fd, &pdu).context("write command")
    }

    pub fn close(&self) {
        unsafe { libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_RDWR) };
    }

    /// Asks the radio for a 7.5 ms connection interval (BlueZ connects at
    /// 30 ms → ~33 Hz input). Sends HCI LE Connection Update on a raw HCI
    /// socket, which needs CAP_NET_RAW (`setcap cap_net_raw+ep` on the binary).
    pub fn request_fast_interval(&self) -> Result<()> {
        const SOL_L2CAP: i32 = 6;
        const L2CAP_CONNINFO: i32 = 0x02;
        const BTPROTO_HCI: i32 = 1;
        const HCI_CHANNEL_RAW: u16 = 0;
        // struct l2cap_conninfo { u16 hci_handle; u8 dev_class[3]; }
        let mut info = [0u8; 6];
        let mut len = info.len() as u32;
        if unsafe {
            libc::getsockopt(self.fd.as_raw_fd(), SOL_L2CAP, L2CAP_CONNINFO, info.as_mut_ptr().cast(), &mut len)
        } < 0
        {
            bail!("L2CAP_CONNINFO: {}", std::io::Error::last_os_error());
        }
        let handle = u16::from_le_bytes([info[0], info[1]]);

        let raw = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW | libc::SOCK_CLOEXEC, BTPROTO_HCI) };
        if raw < 0 {
            bail!("HCI socket: {}", std::io::Error::last_os_error());
        }
        let hci = unsafe { OwnedFd::from_raw_fd(raw) };
        // struct sockaddr_hci { sa_family_t family; u16 dev; u16 channel; } — hci0
        let addr: [u16; 3] = [AF_BLUETOOTH as u16, 0, HCI_CHANNEL_RAW];
        if unsafe { libc::bind(hci.as_raw_fd(), addr.as_ptr().cast(), 6) } < 0 {
            bail!("HCI bind: {}", std::io::Error::last_os_error());
        }
        // LE Connection Update (OGF 0x08, OCF 0x0013): interval 6..6 (7.5 ms),
        // latency 0, supervision timeout 100 (1 s), CE length 0..0.
        let p = |v: u16| v.to_le_bytes();
        let mut pkt = vec![0x01, 0x13, 0x20, 14];
        for v in [handle, 6, 6, 0, 100, 0, 0] {
            pkt.extend_from_slice(&p(v));
        }
        let n = unsafe { libc::write(hci.as_raw_fd(), pkt.as_ptr().cast(), pkt.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EPERM) {
                bail!("no permission for HCI commands; run: sudo setcap cap_net_raw+ep <binary>");
            }
            bail!("HCI write: {e}");
        }
        info!("requested 7.5 ms connection interval (handle {handle:#06x})");
        Ok(())
    }
}

impl AttEvents {
    /// Next notification/indication as (handle, value); `None` once the link drops.
    pub async fn next(&mut self) -> Option<(u16, Vec<u8>)> {
        loop {
            let p = self.rx.recv().await?;
            if matches!(p[0], NOTIFY | INDICATE) && p.len() >= 3 {
                return Some((u16::from_le_bytes([p[1], p[2]]), p[3..].to_vec()));
            }
        }
    }
}

impl Drop for AttLink {
    fn drop(&mut self) {
        self.close();
    }
}
