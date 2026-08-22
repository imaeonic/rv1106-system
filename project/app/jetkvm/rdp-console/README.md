# JetKVM RDP console prototype

This directory contains the RDP transport daemon for the JetKVM physical console prototype.

## Data path

`mstsc -> TCP/3389 -> jetkvm-rdp -> /run/jetkvm-rdp.sock -> jetkvm_app -> native HDMI/USB`

Video is intended to be zero-transcode: JetKVM's native RV1106 H.264 encoder output is submitted directly to IronRDP's server-side EGFX AVC420 pipeline. Keyboard and mouse events received by RDP are translated to USB HID state and sent to the existing JetKVM USB gadget implementation.

The web/WebRTC transport remains present as a recovery and comparison path during development. RDP and WebRTC video sessions are intentionally treated as mutually exclusive in the prototype.

## Local bridge protocol v1

Each message is `type:u8`, `payload_length:u32-le`, then `payload`.

JetKVM -> RDP daemon:
- `0x02 HELLO_ACK`: protocol version `u16-le`
- `0x05 VIDEO_STATE`: ready `u8`, width `u16`, height `u16`, fps*1000 `u32`, active sessions `u32`
- `0x06 VIDEO_FRAME`: duration-us `u32`, width `u16`, height `u16`, codec `u8` (0=H.264), encoded bytes
- `0x07 ERROR`: UTF-8 diagnostic string

RDP daemon -> JetKVM:
- `0x01 HELLO`: protocol version `u16-le`
- `0x03 VIDEO_START`: codec `u8` (0=H.264)
- `0x04 VIDEO_STOP`
- `0x10 KEYBOARD_STATE`: modifier byte + six HID usages
- `0x11 ABS_MOUSE`: x `u16`, y `u16`, buttons `u8`
- `0x12 REL_MOUSE`: dx `i8`, dy `i8`, buttons `u8`
- `0x13 WHEEL`: vertical `i8`, horizontal `i8`
- `0x14 DESKTOP_REQUEST`: width `u16`, height `u16`

## Build

Host check:

```sh
cargo check
```

The firmware uses a static ARMv7 hard-float musl binary so the daemon does not depend on the appliance's uClibc runtime:

```sh
cargo install cross --locked
cross build --release --target armv7-unknown-linux-musleabihf
```

The firmware Makefile automatically packages the resulting binary from that target directory, or accepts `JETKVM_RDP_BIN=/path/to/jetkvm-rdp`.

## First hardware test

1. Boot a development JetKVM build containing both modified `jetkvm_app` and `jetkvm-rdp`.
2. Confirm the normal web console and SSH are healthy with RDP disabled by default.
3. Enable **RDP Console** under **Advanced → Developer Mode**, then reboot.
4. Confirm `/userdata/jetkvm/rdp.log` reports port 3389 listening and the Unix bridge handshake.
5. Connect HDMI and USB to a test target.
6. From Windows, run `mstsc /v:<jetkvm-ip>`.
7. Confirm live HDMI video, absolute mouse movement and keyboard input.
8. Only after the standard 1080p path is proven, enable generated EDID/multimon experiments through the existing `DESKTOP_REQUEST` hook.

## Development OTA channel

Successful `feature/rdp-console` firmware builds publish an immutable prerelease containing
the complete `jetkvm-v2` `update_ota.tar`. The `rdp-console-latest` prerelease carries the
small JSON feed consumed by the Developer Mode update control in the web UI.

The UI reuses JetKVM's normal OTA confirmation, verification, progress and reboot flow.
It requests the complete system component so `jetkvm_app`, `jetkvm-rdp` and their startup
scripts always move together. Normal automatic updates are disabled after selecting this
development channel. Each manual check cache-busts the moving GitHub feed so a replaced
release asset cannot briefly offer an older development image from the CDN.

## Deliberate prototype limits

- RDP security is basic/no-TLS during initial bring-up. Do not expose port 3389 to an untrusted network.
- One RDP client at a time.
- RDP client-requested desktop geometry is advisory until physical HDMI/capture limits are measured.
- WebRTC is retained until RDP has been proven on hardware.
