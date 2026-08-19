#!/usr/bin/env python3
"""Apply compiler fixes required by the pinned IronRDP server API.

This script is idempotent. It exists so CI can repair the prototype branch using
normal GitHub Actions write permissions, then it can be removed once the source
commit has landed.
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
