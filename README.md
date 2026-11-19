# podbar

A small Bluetooth tray for Wayland bars (waybar and anything else that hosts
StatusNotifierItem icons), written in Rust on top of BlueZ. It replaces
`blueman-applet`. It also has first-class AirPods support, so you don't need
a separate AirPods app.

- **One tray icon.** It shows off, on or connected. Left click opens the menu
  (no window, no second icon).
- **Devices.** Connect or disconnect paired devices with one click. Each one
  shows its battery level when BlueZ knows it.
- **Pairing.** *Add device* scans for 60 s and pairs, trusts and connects the
  device you pick. Pairing codes appear as notifications. podbar is the BlueZ
  agent, and it only accepts pairing requests that you started from the menu.
- **AirPods.** This section only appears while AirPods are connected:
  - battery for each bud and the case (in the menu and the tooltip)
  - noise control: off, transparency, adaptive, noise cancellation
  - conversational awareness
  - ear detection: pauses MPRIS players when a bud comes out, resumes when
    both are back, and turns the audio profile off while neither is worn
  - a notification when the battery is low
- **Light.** It is a static binary of about 3 MB, uses about 3 MB of RSS and
  starts in milliseconds. No GTK and no Qt: the menu is drawn by the bar
  (dbusmenu).

The menu follows the locale (Italian or English).

## Install

```sh
cargo build --release
install -Dm755 target/release/podbar ~/.local/bin/podbar
```

Start it with your session, instead of `blueman-applet`. For example, in
labwc's `autostart`:

```sh
podbar >/dev/null 2>&1 &
```

Runtime requirements: BlueZ 5, a StatusNotifierItem host (for example waybar's
`tray` module), a notification daemon, and `pactl` (PipeWire or PulseAudio)
for the AirPods audio features.

## Command line

The running tray can also be driven from a shell or from keybindings:

```sh
podbar status                                   # AirPods battery, mode, ear state
podbar noise off|transparency|adaptive|anc
podbar ca on|off                                # conversational awareness
podbar ear on|off                               # ear detection (saved in ~/.config/podbar)
```

## How the AirPods part works

AirPods speak Apple's accessory protocol (AAP) over L2CAP PSM `0x1001`. When
a device that advertises the AAP service UUID connects, podbar opens that
channel, sends the handshake, and listens for battery, ear detection, noise
control and conversational awareness packets. The packet layouts come from
[LibrePods](https://github.com/kavishdevar/librepods) (GPL-3.0); podbar is a
from-scratch Rust rewrite of the parts a desktop needs. It does not include
LibrePods' phone relay or hearing-aid features.

## License

GPL-3.0-or-later.
