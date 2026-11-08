# evmacro

Low-latency input macros for Linux, with a minimal settings GUI.

evmacro works at the kernel input layer (evdev + uinput), so it behaves the same on **Wayland and X11**, in any desktop, and in games running natively or through Wine/Proton.

## Features

- **Repeat**: autoclicker / key spammer with interval (or clicks per second), press duration, random jitter and repeat count
- **Sequence**: steps (tap, down, up, wait, type, click at position, mouse move, wheel) with speed and loop count, plus a **recorder** that captures your keys, clicks and timing
- **Type text**: types strings using your real keyboard layout (accents, AltGr symbols, dead keys), with `{Enter}`, `{Ctrl+C}`, `{Wait 100}` tokens
- **Click at screen positions**: jump-click-and-return, move-and-click, or a touch tap that never moves the cursor, with a hover-and-press position picker (KDE Plasma)
- **Remap**: make any key or mouse button act as another key or a combo
- **Hold** or **Toggle** modes, optional modifier keys (e.g. Ctrl + Mouse 5), optional blocking of the trigger key
- Global **enable/disable key** with sound and desktop-notification feedback
- Bind keys by pressing them, including side mouse buttons
- Hot-plugging of keyboards and mice; nothing gets stuck if a device is unplugged mid-macro
- Every change in the GUI applies instantly and is autosaved
- `--headless` mode that live-reloads the config file

## Build

Requires Rust (stable) and `libxkbcommon` (installed on practically every Linux desktop).

```sh
git clone https://github.com/santiadjmc/evmacro && cd evmacro
cargo build --release
./target/release/evmacro
```

## Permissions

evmacro needs to read `/dev/input/event*` and write `/dev/uinput`. On many desktops your session already has access. If the **Devices & Log** tab shows no devices, or startup fails on `/dev/uinput`, run once:

```sh
./setup.sh
```

This adds a udev rule for `/dev/uinput`, loads the `uinput` module at boot and adds you to the `input` group. Log out and back in afterwards. Running evmacro as root is not needed or recommended.

## Usage

```
evmacro                 open the settings GUI (macros run while it is open)
evmacro --headless      run without GUI; reloads the config file when it changes
evmacro --config PATH   use a different config file
evmacro --list-keys     print every key/button name usable in the config
evmacro --test-feedback play the enable/disable feedback
```

Defaults: **F8** toggles macros on/off, **F9** stops a recording.

### Exclusive input

With *Exclusive input* on (default), evmacro grabs your keyboards and mice and forwards their events through one virtual device. This is required to block trigger keys and for remaps, and adds only microseconds of latency. Devices are released automatically when evmacro exits. While grabbed, keyboard lock LEDs (Caps/Num Lock) may not update.

### Typing syntax

| Token | Meaning |
| --- | --- |
| `{Enter}` `{Tab}` `{Esc}` `{F5}` `{Up}` | press a key |
| `{Ctrl+Shift+T}` `{Alt+Tab}` | key combo |
| `{Tab 3}` | press 3 times |
| `{Shift down}` / `{Shift up}` | hold / release |
| `{Wait 250}` | pause in milliseconds |
| `{Click 640 360}` | click there, cursor returns afterwards |
| `{Click 640 360 right}` / `{Click 640 360 stay}` | right click / leave the cursor there |
| `{Tap 640 360}` | touch tap, the cursor does not move |
| `{{` `}}` | literal braces |

The keyboard layout is detected automatically (KDE, systemd-localed, `XKB_DEFAULT_LAYOUT`) and can be set in Settings, e.g. `latam`, `es`, `us(intl)`.

### Clicking at positions

Click steps and `{Click}` / `{Tap}` tokens use screen coordinates in logical pixels. Use **📍 Pick**: hover over the target and press any key. Reading the cursor position and screen size uses KWin scripting, so this currently requires **KDE Plasma** (Wayland or X11).

- **Jump & return**: an absolute virtual pointer moves to the spot, clicks, and puts the cursor back within a few tens of milliseconds. Works in practically every app and game.
- **Touch tap**: a virtual touchscreen taps the spot and the cursor never moves. The target app must accept touch input. On multi-monitor setups touch maps to the output KWin assigns to touchscreens.

## Configuration

Stored at `~/.config/evmacro/config.toml`. The GUI writes it for you, but it is plain TOML:

```toml
toggle_key = "KEY_F8"
exclusive = true

[[macros]]
name = "Autoclicker"
enabled = true
trigger = "BTN_EXTRA"
mode = "hold"
block_trigger = true

[macros.action]
type = "repeat"
keys = ["BTN_LEFT"]
interval_ms = 20.0
press_ms = 5.0
```

Key names are the kernel names (`KEY_A`, `KEY_LEFTCTRL`, `BTN_LEFT`, `BTN_SIDE`, ...); see `evmacro --list-keys`.

## Disclaimer

Many online games forbid macros and automation in their terms of service, and anti-cheat software may detect synthetic input. Use responsibly.

## License

MIT

---

<p align="center">Made with ❤️ by Santiago Morales</p>
