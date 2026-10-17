# framecube-bridge

Use the Nintendo Switch Online (Switch 2) GameCube controller wirelessly on a
**Steam Frame** (SteamOS/Linux) or a **Windows** PC, with the device's own
Bluetooth LE radio. No dongle is needed. The controller appears as a virtual
Xbox 360 pad, so Steam Input and games pick it up like any other controller.

| | Steam Frame / Linux | Windows |
|---|---|---|
| Virtual pad | uinput (xpad-style Xbox 360) | ViGEmBus Xbox 360 |
| Input rate | **~130 Hz** (7.5 ms interval) | ~66 Hz (Windows minimum is 15 ms) |
| Rumble | ✓ | ✓ |
| Toggle | **+ › Launch Program › GameCube Controller** | run the exe |

The executable is called `nso-gc-bridge`.

## Steam Frame install
Download `framecube-bridge-*-linux-aarch64.tar.gz` from
[Releases](../../releases) to the Frame (desktop mode or SSH), then:
```
tar xzf framecube-bridge-*-linux-aarch64.tar.gz
cd framecube-bridge-*-linux-aarch64
sh frame/install.sh
sudo setcap cap_net_raw+ep ~/.local/share/nso-gc-bridge/nso-gc-bridge
```

To use it:
1. Open **+ › Launch Program › GameCube Controller** from the home screen to turn
   the bridge on. You may need to restart Steam once for the entry to appear.
2. Hold **Sync** on the controller until its lights sweep. It connects within a
   second or two.
3. Choose the same entry again to turn the bridge off. It never starts on its own
   after a reboot.

Notes:
- The `setcap` lets the bridge request Bluetooth's fastest connection interval
  (7.5 ms). Without it, input runs at Linux's default of about 33 Hz. Re-run it
  after every reinstall.
- CLI: `~/.local/share/nso-gc-bridge/nsogc toggle|start|stop|status|log`.
- `python3 ~/.local/share/nso-gc-bridge/rumble_test.py` buzzes the controller for
  1 s through the same evdev force-feedback path games use.
- `/dev/uinput` must be writable by your user. On the Frame, the `steamos` user
  can already write to it through the `input` group.

To build from source on the Frame, install Rust with rustup's minimal profile,
then run:
```
cargo build --release && sh frame/install.sh
```

## Windows
Requirements:
- Windows 10 or 11 with a Bluetooth 5.x LE adapter
- The [ViGEmBus](https://github.com/nefarius/ViGEmBus) driver
- **Don't pair the controller in Windows Bluetooth settings.** The bridge runs
  Nintendo's own pairing handshake over GATT. If the controller is paired there,
  remove it first.

```
nso-gc-bridge.exe          # bridge to a virtual Xbox 360 pad (same as `run`)
nso-gc-bridge.exe scan     # list Switch 2 controller adverts (hold Sync)
nso-gc-bridge.exe dump     # GATT table + handshake + raw/decoded input
```

## Options
- Add `--pro` to also accept a Switch 2 Pro Controller. Its rumble isn't
  implemented.
- Set `RUST_LOG=debug` to log every command and reply.
- Keep the sticks and triggers still for the first moment after connecting. Their
  rest positions are calibrated from the first 4 reports.

## Mapping (GameCube → Xbox 360)
| GameCube | Xbox 360 |
|---|---|
| A / B / X / Y | A / B / X / Y |
| Z (ZR) / ZL | RB / LB |
| Analog L / R (full click = 255) | LT / RT |
| Start (+) | Start |
| Home | Guide |
| Capture / C | Back |
| Main stick / C-stick / D-pad | Left stick / right stick / D-pad |

## How it works
1. **Discovery:** the bridge finds the controller by its BLE manufacturer data
   (company `0x0553`, VID `057E`, PID `2073`).
2. **Pairing:** it does not use standard BLE/SMP pairing, which the controller
   rejects. Instead it runs Nintendo's pairing exchange over the command
   characteristic: read device info, exchange addresses and keys, confirm the LTK,
   then set the player LED.
3. **Input:** the controller then streams 63-byte input reports with buttons,
   12-bit sticks and analog triggers.
4. **Linux workaround:** BlueZ's GATT client asks for secondary services, which
   the controller's firmware never answers, so the link stalls and drops. On Linux
   the bridge therefore opens its own ATT socket over L2CAP. The kernel then gives
   the ATT channel to the bridge instead of `bluetoothd`. The bridge discovers only
   the characteristics it needs, and sends an HCI LE Connection Update to request
   the 7.5 ms interval.

## Limitations
- After a disconnect, press Sync again to reconnect. Bonded reconnect (encrypting
  the link with the exchanged LTK) isn't implemented.
- The controller has no gyro, and its rumble is on/off only.
- Windows can't go faster than its 15 ms BLE interval.

## Credits
The Switch 2 protocol logic is ported from
[joypad-os](https://github.com/joypad-ai/joypad-os) (Apache-2.0) and checked
against [ndeadly/switch2_controller_research](https://github.com/ndeadly/switch2_controller_research).
joypad-os in turn builds on [BlueRetro](https://github.com/darthcloud/BlueRetro).
See [NOTICE](NOTICE).

## License
Apache-2.0. Not affiliated with Nintendo or Valve.
