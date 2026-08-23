#!/usr/bin/env python3
"""Apply compiler and transport fixes required by the pinned IronRDP server API.

This script is idempotent. CI uses it to keep the prototype branch source in the
shape required by the pinned IronRDP revision.
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

cargo = ROOT / "project/app/jetkvm/rdp-console/Cargo.toml"
text = cargo.read_text()
marker = 'ironrdp-displaycontrol = { git = "https://github.com/Devolutions/IronRDP", rev = "1723385068ee3a4be635c199c752eb2bbd183f1e", package = "ironrdp-displaycontrol" }\n'
additions = (
    'ironrdp-dvc = { git = "https://github.com/Devolutions/IronRDP", rev = "1723385068ee3a4be635c199c752eb2bbd183f1e", package = "ironrdp-dvc" }\n'
    'ironrdp-svc = { git = "https://github.com/Devolutions/IronRDP", rev = "1723385068ee3a4be635c199c752eb2bbd183f1e", package = "ironrdp-svc" }\n'
)
if 'ironrdp-dvc = ' not in text:
    assert marker in text, "Cargo dependency insertion point not found"
    text = text.replace(marker, marker + additions, 1)
cargo.write_text(text)

main = ROOT / "project/app/jetkvm/rdp-console/src/main.rs"
text = main.read_text()

old = 'use bytes::Bytes;\nuse ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;'
new = 'use bytes::Bytes;\nuse ironrdp_dvc::encode_dvc_messages;\nuse ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;\nuse ironrdp_svc::ChannelFlags;'
if 'use ironrdp_dvc::encode_dvc_messages;' not in text:
    assert old in text, "Rust import insertion point not found"
    text = text.replace(old, new, 1)

# MS-RDPEGFX AVC420 requires the H.264 Annex-B byte-stream format. JetKVM
# already emits Annex-B, so pass the native frame through unchanged.

old = '''        let messages = gfx.drain_output();
        drop(gfx);
        if !messages.is_empty() {
            let _ = sender.send(ServerEvent::Egfx(EgfxServerMessage::SendMessages { messages }));
        }
'''
new = '''        let Some(channel_id) = gfx.channel_id() else {
            return;
        };
        let dvc_messages = gfx.drain_output();
        let messages = match encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL) {
            Ok(messages) => messages,
            Err(error) => {
                warn!(?error, "failed to encode EGFX DVC messages");
                return;
            }
        };
        drop(gfx);
        if !messages.is_empty() {
            let _ = sender.send(ServerEvent::Egfx(EgfxServerMessage::SendMessages { messages }));
        }
'''
if 'let dvc_messages = gfx.drain_output();' not in text:
    assert old in text, "EGFX output block not found"
    text = text.replace(old, new, 1)

# Do not start the native encoder when the TCP connection is merely accepted.
# EGFX negotiation takes hundreds of milliseconds, and starting here causes the
# initial SPS/PPS/IDR frame to be dropped before the graphics pipeline is ready.
old = '''struct JetKvmGfxHandler;

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
'''
new = '''struct JetKvmGfxHandler {
    bridge: BridgeLink,
}

impl GraphicsPipelineHandler for JetKvmGfxHandler {
    fn capabilities_advertise(&mut self, pdu: &CapabilitiesAdvertisePdu) {
        debug!(?pdu, "RDP client advertised EGFX capabilities");
    }

    fn on_ready(&mut self, negotiated: &CapabilitySet) {
        info!(?negotiated, "RDP EGFX pipeline ready");
        info!("requesting JetKVM video after EGFX became ready");
        self.bridge.start_video();
    }
}

struct JetKvmGfxFactory {
    shared: GfxShared,
    bridge: BridgeLink,
}
'''
if 'requesting JetKVM video after EGFX became ready' not in text:
    assert old in text, "EGFX handler block not found"
    text = text.replace(old, new, 1)

old = '''    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(JetKvmGfxHandler)
    }

    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        let handle = Arc::new(Mutex::new(GraphicsPipelineServer::new(Box::new(
            JetKvmGfxHandler,
        ))));
'''
new = '''    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(JetKvmGfxHandler {
            bridge: self.bridge.clone(),
        })
    }

    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        let handle = Arc::new(Mutex::new(GraphicsPipelineServer::new(Box::new(
            JetKvmGfxHandler {
                bridge: self.bridge.clone(),
            },
        ))));
'''
if 'bridge: self.bridge.clone(),' not in text:
    assert old in text, "EGFX factory handler construction block not found"
    text = text.replace(old, new, 1)

old = '''    fn on_accept(&mut self, peer: SocketAddr) -> bool {
        info!(%peer, "RDP client connected");
        self.bridge.start_video();
        true
    }
'''
new = '''    fn on_accept(&mut self, peer: SocketAddr) -> bool {
        info!(%peer, "RDP client connected");
        true
    }
'''
if 'self.bridge.start_video();\n        true' in text:
    assert old in text, "RDP on_accept block not found"
    text = text.replace(old, new, 1)

old = '    let gfx_factory = JetKvmGfxFactory { shared: gfx };'
new = '''    let gfx_factory = JetKvmGfxFactory {
        shared: gfx,
        bridge: bridge.clone(),
    };'''
if 'bridge: bridge.clone(),' not in text:
    assert old in text, "EGFX factory construction point not found"
    text = text.replace(old, new, 1)

old = '                    debug!(?*state, "JetKVM video state");'
if old in text:
    text = text.replace(old, '                    debug!(state = ?state, "JetKVM video state");', 1)

old = '    tracing_subscriber::fmt().with_env_filter(filter).compact().try_init()?;'
new = '''    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialise tracing: {error}"))?;'''
if old in text:
    text = text.replace(old, new, 1)

main.write_text(text)
