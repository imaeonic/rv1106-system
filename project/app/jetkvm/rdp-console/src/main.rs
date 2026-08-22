use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_dvc::encode_dvc_messages;
use ironrdp_egfx::pdu::{Avc420Region, CapabilitiesAdvertisePdu, CapabilitySet};
use ironrdp_egfx::server::{GraphicsPipelineHandler, GraphicsPipelineServer};
use ironrdp_server::tokio;
use ironrdp_server::{
    BitmapUpdate, DesktopSize, DisplayUpdate, EgfxServerMessage, GfxDvcBridge, GfxServerFactory,
    GfxServerHandle, KeyboardEvent, MouseEvent, PixelFormat, PostConnectionAction, RdpServer,
    RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerEvent,
    ServerEventSender,
};
use ironrdp_svc::ChannelFlags;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::EnvFilter;

const PROTOCOL_VERSION: u16 = 1;
const DEFAULT_SOCKET: &str = "/run/jetkvm-rdp.sock";
const DEFAULT_BIND: &str = "0.0.0.0:3389";
const DEFAULT_WIDTH: u16 = 1920;
const DEFAULT_HEIGHT: u16 = 1080;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

const MSG_HELLO: u8 = 0x01;
const MSG_HELLO_ACK: u8 = 0x02;
const MSG_VIDEO_START: u8 = 0x03;
const MSG_VIDEO_STOP: u8 = 0x04;
const MSG_VIDEO_STATE: u8 = 0x05;
const MSG_VIDEO_FRAME: u8 = 0x06;
const MSG_ERROR: u8 = 0x07;
const MSG_KEYBOARD_STATE: u8 = 0x10;
const MSG_ABS_MOUSE: u8 = 0x11;
const MSG_REL_MOUSE: u8 = 0x12;
const MSG_WHEEL: u8 = 0x13;
const MSG_DESKTOP_REQUEST: u8 = 0x14;

#[derive(Clone, Copy, Debug)]
struct VideoState {
    ready: bool,
    width: u16,
    height: u16,
    fps_milli: u32,
}

impl Default for VideoState {
    fn default() -> Self {
        Self {
            ready: false,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            fps_milli: 0,
        }
    }
}

#[derive(Clone)]
struct BridgeLink {
    tx: Arc<Mutex<Option<UnboundedSender<BridgeMessage>>>>,
    video: Arc<Mutex<VideoState>>,
}

impl BridgeLink {
    fn new() -> Self {
        Self {
            tx: Arc::new(Mutex::new(None)),
            video: Arc::new(Mutex::new(VideoState::default())),
        }
    }

    fn set_tx(&self, tx: Option<UnboundedSender<BridgeMessage>>) {
        *self.tx.lock().expect("bridge tx mutex poisoned") = tx;
    }

    fn send(&self, typ: u8, payload: Vec<u8>) {
        if let Some(tx) = self.tx.lock().expect("bridge tx mutex poisoned").as_ref() {
            let _ = tx.send(BridgeMessage { typ, payload });
        }
    }

    fn start_video(&self) {
        self.send(MSG_VIDEO_START, vec![0]);
    }

    fn stop_video(&self) {
        self.send(MSG_VIDEO_STOP, Vec::new());
        self.keyboard_state(0, [0; 6]);
    }

    fn keyboard_state(&self, modifiers: u8, keys: [u8; 6]) {
        let mut payload = Vec::with_capacity(7);
        payload.push(modifiers);
        payload.extend_from_slice(&keys);
        self.send(MSG_KEYBOARD_STATE, payload);
    }

    fn abs_mouse(&self, x: u16, y: u16, buttons: u8) {
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&x.to_le_bytes());
        payload.extend_from_slice(&y.to_le_bytes());
        payload.push(buttons);
        self.send(MSG_ABS_MOUSE, payload);
    }

    fn rel_mouse(&self, dx: i8, dy: i8, buttons: u8) {
        self.send(MSG_REL_MOUSE, vec![dx as u8, dy as u8, buttons]);
    }

    fn wheel(&self, vertical: i8, horizontal: i8) {
        self.send(MSG_WHEEL, vec![vertical as u8, horizontal as u8]);
    }

    fn request_desktop(&self, size: DesktopSize) {
        let mut payload = Vec::with_capacity(4);
        payload.extend_from_slice(&size.width.to_le_bytes());
        payload.extend_from_slice(&size.height.to_le_bytes());
        self.send(MSG_DESKTOP_REQUEST, payload);
    }

    fn desktop_size(&self) -> DesktopSize {
        let state = *self.video.lock().expect("video state mutex poisoned");
        DesktopSize {
            width: state.width.max(200),
            height: state.height.max(200),
        }
    }
}

struct BridgeMessage {
    typ: u8,
    payload: Vec<u8>,
}

struct VideoFrame {
    duration_us: u32,
    width: u16,
    height: u16,
    codec: u8,
    data: Vec<u8>,
}

#[derive(Clone)]
struct GfxShared {
    inner: Arc<GfxSharedInner>,
}

struct GfxSharedInner {
    handle: Mutex<Option<GfxServerHandle>>,
    sender: Mutex<Option<UnboundedSender<ServerEvent>>>,
    surface: Mutex<Option<(u16, u16, u16)>>,
    epoch: Instant,
}

impl GfxShared {
    fn new() -> Self {
        Self {
            inner: Arc::new(GfxSharedInner {
                handle: Mutex::new(None),
                sender: Mutex::new(None),
                surface: Mutex::new(None),
                epoch: Instant::now(),
            }),
        }
    }

    fn set_handle(&self, handle: GfxServerHandle) {
        *self.inner.handle.lock().expect("gfx handle mutex poisoned") = Some(handle);
        *self
            .inner
            .surface
            .lock()
            .expect("gfx surface mutex poisoned") = None;
    }

    fn set_sender(&self, sender: UnboundedSender<ServerEvent>) {
        *self.inner.sender.lock().expect("gfx sender mutex poisoned") = Some(sender);
    }

    fn submit_h264(&self, frame: VideoFrame) {
        if frame.codec != 0 || frame.width == 0 || frame.height == 0 || frame.data.is_empty() {
            return;
        }

        let handle = self
            .inner
            .handle
            .lock()
            .expect("gfx handle mutex poisoned")
            .clone();
        let sender = self
            .inner
            .sender
            .lock()
            .expect("gfx sender mutex poisoned")
            .clone();
        let (Some(handle), Some(sender)) = (handle, sender) else {
            return;
        };

        let mut gfx = handle.lock().expect("gfx server mutex poisoned");
        if !gfx.is_ready() || !gfx.supports_avc420() {
            return;
        }

        let current_surface = *self
            .inner
            .surface
            .lock()
            .expect("gfx surface mutex poisoned");
        let surface_id = match current_surface {
            Some((id, width, height)) if width == frame.width && height == frame.height => id,
            old => {
                if let Some((old_id, _, _)) = old {
                    let _ = gfx.delete_surface(old_id);
                    gfx.resize(frame.width, frame.height);
                } else {
                    gfx.set_output_dimensions(frame.width, frame.height);
                }

                let Some(id) = gfx.create_surface(frame.width, frame.height) else {
                    return;
                };
                if !gfx.map_surface_to_output(id, 0, 0) {
                    return;
                }
                *self
                    .inner
                    .surface
                    .lock()
                    .expect("gfx surface mutex poisoned") = Some((id, frame.width, frame.height));
                id
            }
        };

        let regions = [Avc420Region::full_frame(frame.width, frame.height, 22)];
        let timestamp_ms = self
            .inner
            .epoch
            .elapsed()
            .as_millis()
            .min(u128::from(u32::MAX)) as u32;
        let queued = gfx
            .send_avc420_frame(surface_id, &frame.data, &regions, timestamp_ms)
            .is_some();

        if !queued {
            trace!(
                frames_in_flight = gfx.frames_in_flight(),
                duration_us = frame.duration_us,
                "dropping H264 frame due to EGFX state/backpressure"
            );
            return;
        }

        let Some(channel_id) = gfx.channel_id() else {
            return;
        };
        let dvc_messages = gfx.drain_output();
        let messages =
            match encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL) {
                Ok(messages) => messages,
                Err(error) => {
                    warn!(?error, "failed to encode EGFX DVC messages");
                    return;
                }
            };
        drop(gfx);
        if !messages.is_empty() {
            let _ = sender.send(ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                messages,
            }));
        }
    }
}

struct JetKvmGfxHandler;

impl GraphicsPipelineHandler for JetKvmGfxHandler {
    fn capabilities_advertise(&mut self, pdu: &CapabilitiesAdvertisePdu) {
        debug!(?pdu, "RDP client advertised EGFX capabilities");
    }

    fn on_ready(&mut self, negotiated: &CapabilitySet) {
        info!(?negotiated, "RDP EGFX pipeline ready");
    }
}

struct JetKvmGfxFactory {
    shared: GfxShared,
}

impl ServerEventSender for JetKvmGfxFactory {
    fn set_sender(&mut self, sender: UnboundedSender<ServerEvent>) {
        self.shared.set_sender(sender);
    }
}

impl GfxServerFactory for JetKvmGfxFactory {
    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(JetKvmGfxHandler)
    }

    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        let handle = Arc::new(Mutex::new(GraphicsPipelineServer::new(Box::new(
            JetKvmGfxHandler,
        ))));
        self.shared.set_handle(handle.clone());
        Some((GfxDvcBridge::new(handle.clone()), handle))
    }
}

#[derive(Clone)]
struct InputHandler {
    bridge: BridgeLink,
    state: Arc<Mutex<InputState>>,
}

#[derive(Default)]
struct InputState {
    modifiers: u8,
    keys: BTreeSet<u8>,
    mouse_x: u16,
    mouse_y: u16,
    mouse_buttons: u8,
}

impl InputHandler {
    fn new(bridge: BridgeLink) -> Self {
        Self {
            bridge,
            state: Arc::new(Mutex::new(InputState::default())),
        }
    }

    fn send_keyboard(&self, state: &InputState) {
        let mut keys = [0u8; 6];
        for (dst, src) in keys.iter_mut().zip(state.keys.iter().take(6)) {
            *dst = *src;
        }
        self.bridge.keyboard_state(state.modifiers, keys);
    }

    fn set_button(&self, mask: u8, down: bool) {
        let mut state = self.state.lock().expect("input state mutex poisoned");
        if down {
            state.mouse_buttons |= mask;
        } else {
            state.mouse_buttons &= !mask;
        }
        let (x, y) = rdp_to_hid_coords(state.mouse_x, state.mouse_y, self.bridge.desktop_size());
        self.bridge.abs_mouse(x, y, state.mouse_buttons);
    }
}

impl RdpServerInputHandler for InputHandler {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let mut state = self.state.lock().expect("input state mutex poisoned");
        match event {
            KeyboardEvent::Pressed { code, extended } => {
                if let Some(modifier) = scancode_modifier(code, extended) {
                    state.modifiers |= modifier;
                } else if let Some(usage) = scancode_to_hid(code, extended) {
                    state.keys.insert(usage);
                }
                self.send_keyboard(&state);
            }
            KeyboardEvent::Released { code, extended } => {
                if let Some(modifier) = scancode_modifier(code, extended) {
                    state.modifiers &= !modifier;
                } else if let Some(usage) = scancode_to_hid(code, extended) {
                    state.keys.remove(&usage);
                }
                self.send_keyboard(&state);
            }
            KeyboardEvent::UnicodePressed(code) | KeyboardEvent::UnicodeReleased(code) => {
                trace!(
                    code,
                    "Unicode RDP keyboard event ignored; mstsc normally sends scan codes"
                );
            }
            KeyboardEvent::Synchronize(flags) => {
                trace!(?flags, "RDP keyboard synchronize");
            }
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        match event {
            MouseEvent::Move { x, y } => {
                let mut state = self.state.lock().expect("input state mutex poisoned");
                state.mouse_x = x;
                state.mouse_y = y;
                let (hx, hy) = rdp_to_hid_coords(x, y, self.bridge.desktop_size());
                self.bridge.abs_mouse(hx, hy, state.mouse_buttons);
            }
            MouseEvent::LeftPressed => self.set_button(0x01, true),
            MouseEvent::LeftReleased => self.set_button(0x01, false),
            MouseEvent::RightPressed => self.set_button(0x02, true),
            MouseEvent::RightReleased => self.set_button(0x02, false),
            MouseEvent::MiddlePressed => self.set_button(0x04, true),
            MouseEvent::MiddleReleased => self.set_button(0x04, false),
            MouseEvent::Button4Pressed => self.set_button(0x08, true),
            MouseEvent::Button4Released => self.set_button(0x08, false),
            MouseEvent::Button5Pressed => self.set_button(0x10, true),
            MouseEvent::Button5Released => self.set_button(0x10, false),
            MouseEvent::VerticalScroll { value } => self.bridge.wheel(wheel_units(value), 0),
            MouseEvent::Scroll { x, y } => {
                self.bridge.wheel(
                    wheel_units(y.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16),
                    wheel_units(x.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16),
                );
            }
            MouseEvent::RelMove { x, y } => {
                let dx = x.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
                let dy = y.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
                let buttons = self
                    .state
                    .lock()
                    .expect("input state mutex poisoned")
                    .mouse_buttons;
                self.bridge.rel_mouse(dx, dy, buttons);
            }
        }
    }
}

fn wheel_units(value: i16) -> i8 {
    if value == 0 {
        return 0;
    }
    let value = i32::from(value);
    let steps = if value.unsigned_abs() >= 120 {
        value / 120
    } else {
        value.signum()
    };
    steps.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

fn rdp_to_hid_coords(x: u16, y: u16, size: DesktopSize) -> (u16, u16) {
    fn scale(value: u16, extent: u16) -> u16 {
        if extent <= 1 {
            return 0;
        }
        ((u32::from(value.min(extent - 1)) * 32767) / u32::from(extent - 1)) as u16
    }
    (scale(x, size.width), scale(y, size.height))
}

fn scancode_modifier(code: u8, extended: bool) -> Option<u8> {
    match (code, extended) {
        (0x1d, false) => Some(0x01),
        (0x2a, false) => Some(0x02),
        (0x38, false) => Some(0x04),
        (0x5b, true) => Some(0x08),
        (0x1d, true) => Some(0x10),
        (0x36, false) => Some(0x20),
        (0x38, true) => Some(0x40),
        (0x5c, true) => Some(0x80),
        _ => None,
    }
}

fn scancode_to_hid(code: u8, extended: bool) -> Option<u8> {
    if extended {
        return match code {
            0x1c => Some(0x58),
            0x35 => Some(0x54),
            0x47 => Some(0x4a),
            0x48 => Some(0x52),
            0x49 => Some(0x4b),
            0x4b => Some(0x50),
            0x4d => Some(0x4f),
            0x4f => Some(0x4d),
            0x50 => Some(0x51),
            0x51 => Some(0x4e),
            0x52 => Some(0x49),
            0x53 => Some(0x4c),
            0x5d => Some(0x65),
            _ => None,
        };
    }

    match code {
        0x01 => Some(0x29),
        0x02..=0x0b => Some(0x1e + ((code - 0x02) % 10)),
        0x0c => Some(0x2d),
        0x0d => Some(0x2e),
        0x0e => Some(0x2a),
        0x0f => Some(0x2b),
        0x10 => Some(0x14),
        0x11 => Some(0x1a),
        0x12 => Some(0x08),
        0x13 => Some(0x15),
        0x14 => Some(0x17),
        0x15 => Some(0x1c),
        0x16 => Some(0x18),
        0x17 => Some(0x0c),
        0x18 => Some(0x12),
        0x19 => Some(0x13),
        0x1a => Some(0x2f),
        0x1b => Some(0x30),
        0x1c => Some(0x28),
        0x1e => Some(0x04),
        0x1f => Some(0x16),
        0x20 => Some(0x07),
        0x21 => Some(0x09),
        0x22 => Some(0x0a),
        0x23 => Some(0x0b),
        0x24 => Some(0x0d),
        0x25 => Some(0x0e),
        0x26 => Some(0x0f),
        0x27 => Some(0x33),
        0x28 => Some(0x34),
        0x29 => Some(0x35),
        0x2b => Some(0x31),
        0x2c => Some(0x1d),
        0x2d => Some(0x1b),
        0x2e => Some(0x06),
        0x2f => Some(0x19),
        0x30 => Some(0x05),
        0x31 => Some(0x11),
        0x32 => Some(0x10),
        0x33 => Some(0x36),
        0x34 => Some(0x37),
        0x35 => Some(0x38),
        0x37 => Some(0x55),
        0x39 => Some(0x2c),
        0x3a => Some(0x39),
        0x3b..=0x44 => Some(0x3a + (code - 0x3b)),
        0x45 => Some(0x53),
        0x46 => Some(0x47),
        0x47 => Some(0x5f),
        0x48 => Some(0x60),
        0x49 => Some(0x61),
        0x4a => Some(0x56),
        0x4b => Some(0x5c),
        0x4c => Some(0x5d),
        0x4d => Some(0x5e),
        0x4e => Some(0x57),
        0x4f => Some(0x59),
        0x50 => Some(0x5a),
        0x51 => Some(0x5b),
        0x52 => Some(0x62),
        0x53 => Some(0x63),
        0x57 => Some(0x44),
        0x58 => Some(0x45),
        _ => None,
    }
}

#[derive(Clone)]
struct DisplayHandler {
    bridge: BridgeLink,
}

struct DisplayUpdates {
    sent_probe: bool,
}

#[async_trait]
impl RdpServerDisplayUpdates for DisplayUpdates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        if !self.sent_probe {
            self.sent_probe = true;
            let width = NonZeroU16::new(64).expect("64 != 0");
            let height = NonZeroU16::new(64).expect("64 != 0");
            let stride = NonZeroUsize::new(64 * 4).expect("stride != 0");
            let mut data = vec![0u8; 64 * 64 * 4];
            for y in 0..64usize {
                for x in 0..64usize {
                    let offset = (y * 64 + x) * 4;
                    let value = if ((x / 8) + (y / 8)) % 2 == 0 {
                        0x24
                    } else {
                        0x38
                    };
                    data[offset] = value;
                    data[offset + 1] = value;
                    data[offset + 2] = value;
                    data[offset + 3] = 0xff;
                }
            }
            return Ok(Some(DisplayUpdate::Bitmap(BitmapUpdate {
                x: 0,
                y: 0,
                width,
                height,
                format: PixelFormat::BgrA32,
                data: Bytes::from(data),
                stride,
            })));
        }

        std::future::pending::<()>().await;
        Ok(None)
    }
}

#[async_trait]
impl RdpServerDisplay for DisplayHandler {
    async fn size(&mut self) -> DesktopSize {
        self.bridge.desktop_size()
    }

    async fn request_initial_size(&mut self, client_size: DesktopSize) -> DesktopSize {
        self.bridge.request_desktop(client_size);
        self.bridge.desktop_size()
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(DisplayUpdates { sent_probe: false }))
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        debug!(?layout, "RDP client requested monitor layout");
    }
}

struct ConnectionLifecycle {
    bridge: BridgeLink,
}

impl ironrdp_server::ConnectionHandler for ConnectionLifecycle {
    fn on_accept(&mut self, peer: SocketAddr) -> bool {
        info!(%peer, "RDP client connected");
        self.bridge.start_video();
        true
    }

    fn on_disconnected(
        &mut self,
        peer: SocketAddr,
        duration: std::time::Duration,
        error: Option<&anyhow::Error>,
    ) -> PostConnectionAction {
        if let Some(error) = error {
            warn!(%peer, ?duration, %error, "RDP client disconnected with error");
        } else {
            info!(%peer, ?duration, "RDP client disconnected");
        }
        self.bridge.stop_video();
        PostConnectionAction::Continue
    }
}

async fn bridge_supervisor(socket_path: String, bridge: BridgeLink, gfx: GfxShared) {
    loop {
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                info!(path = %socket_path, "connected to JetKVM local bridge");
                if let Err(error) = run_bridge_connection(stream, bridge.clone(), gfx.clone()).await
                {
                    warn!(%error, "JetKVM local bridge connection ended");
                }
                bridge.set_tx(None);
            }
            Err(error) => debug!(path = %socket_path, %error, "waiting for JetKVM local bridge"),
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn run_bridge_connection(
    stream: UnixStream,
    bridge: BridgeLink,
    gfx: GfxShared,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<BridgeMessage>();
    bridge.set_tx(Some(tx));
    bridge.send(MSG_HELLO, PROTOCOL_VERSION.to_le_bytes().to_vec());

    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if let Err(error) = write_message(&mut writer, message.typ, &message.payload).await {
                debug!(%error, "bridge writer stopped");
                break;
            }
        }
    });

    let result = async {
        loop {
            let (typ, payload) = read_message(&mut reader).await?;
            match typ {
                MSG_HELLO_ACK => {
                    if payload.len() != 2 || u16::from_le_bytes([payload[0], payload[1]]) != PROTOCOL_VERSION {
                        anyhow::bail!("JetKVM bridge protocol mismatch");
                    }
                    info!(version = PROTOCOL_VERSION, "JetKVM bridge handshake complete");
                }
                MSG_VIDEO_STATE => {
                    if payload.len() != 13 {
                        anyhow::bail!("invalid VIDEO_STATE length {}", payload.len());
                    }
                    let mut state = bridge.video.lock().expect("video state mutex poisoned");
                    state.ready = payload[0] != 0;
                    let width = u16::from_le_bytes([payload[1], payload[2]]);
                    let height = u16::from_le_bytes([payload[3], payload[4]]);
                    if width > 0 { state.width = width; }
                    if height > 0 { state.height = height; }
                    state.fps_milli = u32::from_le_bytes(payload[5..9].try_into().expect("4 bytes"));
                    debug!(state = ?state, "JetKVM video state");
                }
                MSG_VIDEO_FRAME => {
                    if payload.len() < 9 { continue; }
                    gfx.submit_h264(VideoFrame {
                        duration_us: u32::from_le_bytes(payload[0..4].try_into().expect("4 bytes")),
                        width: u16::from_le_bytes(payload[4..6].try_into().expect("2 bytes")),
                        height: u16::from_le_bytes(payload[6..8].try_into().expect("2 bytes")),
                        codec: payload[8],
                        data: payload[9..].to_vec(),
                    });
                }
                MSG_ERROR => warn!(message = %String::from_utf8_lossy(&payload), "JetKVM bridge reported error"),
                other => trace!(typ = other, len = payload.len(), "ignored bridge message"),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    }.await;

    writer_task.abort();
    result
}

async fn read_message<R>(reader: &mut R) -> Result<(u8, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let typ = reader.read_u8().await.context("read bridge message type")?;
    let length = reader
        .read_u32_le()
        .await
        .context("read bridge message length")? as usize;
    if length > MAX_MESSAGE {
        anyhow::bail!("bridge message too large: {length}");
    }
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .await
        .context("read bridge message payload")?;
    Ok((typ, payload))
}

async fn write_message<W>(writer: &mut W, typ: u8, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_MESSAGE {
        anyhow::bail!("bridge message too large: {}", payload.len());
    }
    writer.write_u8(typ).await?;
    writer.write_u32_le(payload.len() as u32).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

fn setup_logging() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialise tracing: {error}"))?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    setup_logging()?;

    let bind: SocketAddr = std::env::var("JETKVM_RDP_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()
        .context("parse JETKVM_RDP_BIND")?;
    let socket = std::env::var("JETKVM_RDP_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_owned());

    let bridge = BridgeLink::new();
    let gfx = GfxShared::new();
    tokio::spawn(bridge_supervisor(socket, bridge.clone(), gfx.clone()));

    let input = InputHandler::new(bridge.clone());
    let display = DisplayHandler {
        bridge: bridge.clone(),
    };
    let gfx_factory = JetKvmGfxFactory { shared: gfx };

    let mut server = RdpServer::builder()
        .with_addr(bind)
        .with_no_security()
        .with_input_handler(input)
        .with_display_handler(display)
        .with_gfx_factory(Some(Box::new(gfx_factory)))
        .with_connection_handler(Some(Box::new(ConnectionLifecycle { bridge })))
        .build();

    info!(%bind, "JetKVM RDP console listening");
    if let Err(error) = server.run().await {
        error!(%error, "RDP server stopped");
        return Err(error);
    }
    Ok(())
}
