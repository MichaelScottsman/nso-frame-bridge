//! nso-gc-bridge: Nintendo Switch Online (Switch 2) GameCube controller →
//! virtual Xbox 360 pad, over the PC's own Bluetooth LE adapter.

#[cfg(target_os = "linux")]
mod att;
mod ble;
mod pad;
mod protocol;
#[cfg(windows)]
mod winconn;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use log::{error, info, warn};
use protocol::{Calibrator, PID_GC, PID_PRO2, hex, parse_report};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Also accept a Switch 2 Pro Controller.
    #[arg(long, global = true)]
    pro: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// List Nintendo Switch 2 controller adverts (press Sync on the pad).
    Scan {
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
    /// Connect, print the GATT table, run the handshake and show raw/decoded input.
    Dump,
    /// Bridge the controller to a virtual Xbox 360 pad (default).
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let want: &[u16] = if cli.pro { &[PID_GC, PID_PRO2] } else { &[PID_GC] };
    let adapter = ble::adapter().await?;

    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Scan { secs } => {
            println!("Scanning {secs}s — hold the Sync button on the controller…");
            let mut events = ble::events(&adapter).await?;
            ble::find_controller(&adapter, &mut events, want, Some(Duration::from_secs(secs)), true).await?;
        }
        Cmd::Dump => dump(&adapter, want).await?,
        Cmd::Run => run(&adapter, want).await?,
    }
    Ok(())
}

async fn connect(adapter: &btleplug::platform::Adapter, events: &mut ble::Events, want: &[u16]) -> Result<ble::Session> {
    info!("waiting for controller — press the Sync button (or any button if it was paired before)");
    let Some((p, pid)) = ble::find_controller(adapter, events, want, None, false).await? else {
        bail!("scan ended without finding a controller");
    };
    ble::Session::connect(p, pid).await
}

async fn dump(adapter: &btleplug::platform::Adapter, want: &[u16]) -> Result<()> {
    let host = ble::host_address(adapter).await?;
    let mut events = ble::events(adapter).await?;
    let s = connect(adapter, &mut events, want).await?;
    s.print_gatt();
    let mut cal = Calibrator::new(s.pid);
    let mut last = Vec::new();
    let res = s
        .run(
            host,
            |raw| {
                // Only print when something other than the counter/timestamp bytes changes.
                let key: Vec<u8> = raw.iter().skip(4).copied().collect();
                if key == last {
                    return;
                }
                last = key;
                let decoded = parse_report(raw).and_then(|r| cal.apply(&r).map(|st| (r, st)));
                match decoded {
                    Some((r, st)) => println!(
                        "[{:2}] btn={:08x} L=({:4},{:4}) C=({:4},{:4}) LT={:3} RT={:3} | lx={:+.2} ly={:+.2} cx={:+.2} cy={:+.2} lt={:3} rt={:3}\n     {}",
                        raw.len(), r.buttons, r.lx, r.ly, r.rx, r.ry, r.lt, r.rt,
                        st.lx, st.ly, st.rx, st.ry, st.lt, st.rt, hex(raw)
                    ),
                    None => println!("[{:2}] (calibrating) {}", raw.len(), hex(raw)),
                }
            },
            None,
        )
        .await;
    s.disconnect().await;
    res
}

async fn run(adapter: &btleplug::platform::Adapter, want: &[u16]) -> Result<()> {
    let host = ble::host_address(adapter).await?;
    let (rumble_tx, rumble_rx) = watch::channel(0u8);
    let mut pad = pad::Pad::new(rumble_tx)?;
    info!("virtual Xbox 360 pad plugged in");
    let mut events = ble::events(adapter).await?;

    loop {
        let s = tokio::select! {
            s = connect(adapter, &mut events, want) => s,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let s = match s {
            Ok(s) => s,
            Err(e) => {
                warn!("connect failed: {e:#}; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut cal = Calibrator::new(s.pid);
        let res = tokio::select! {
            r = s.run(
                host,
                |raw| {
                    if let Some(st) = parse_report(raw).and_then(|r| cal.apply(&r))
                        && let Err(e) = pad.update(&st)
                    {
                        error!("{e:#}");
                    }
                },
                Some(rumble_rx.clone()),
            ) => r,
            _ = tokio::signal::ctrl_c() => {
                s.disconnect().await;
                return Ok(());
            }
        };
        pad.neutral().ok();
        s.disconnect().await;
        warn!("{}", res.err().map(|e| format!("{e:#}")).unwrap_or_default());
        info!("reconnecting…");
    }
}
