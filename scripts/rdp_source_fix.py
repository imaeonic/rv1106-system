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

old = 'use ironrdp_egfx::pdu::{Avc420Region, CapabilitiesAdvertisePdu, CapabilitySet};'
new = 'use ironrdp_egfx::pdu::{annex_b_to_avc, Avc420Region, CapabilitiesAdvertisePdu, CapabilitySet};'
if 'annex_b_to_avc' not in text:
    assert old in text, "AVC converter import insertion point not found"
    text = text.replace(old, new, 1)

old = '''        let queued = gfx
            .send_avc420_frame(surface_id, &frame.data, &regions, timestamp_ms)
            .is_some();
'''
new = '''        // JetKVM's native encoder emits Annex-B start-code-prefixed NAL units.
        // MS-RDPEGFX AVC420 carries AVC/AVCC length-prefixed NAL units, so convert
        // each complete native frame before handing it to IronRDP.
        let avc_data = annex_b_to_avc(&frame.data);
        if avc_data.is_empty() {
            warn!(input_bytes = frame.data.len(), "Annex-B to AVC conversion produced an empty frame");
            return;
        }
        let queued = gfx
            .send_avc420_frame(surface_id, &avc_data, &regions, timestamp_ms)
            .is_some();
'''
if 'let avc_data = annex_b_to_avc(&frame.data);' not in text:
    assert old in text, "AVC frame submission block not found"
    text = text.replace(old, new, 1)

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
