use std::env;
use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

const PROTOCOL_VERSION: u16 = 1;
const DEFAULT_SOCKET: &str = "/run/jetkvm-rdp.sock";
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

const MSG_HELLO: u8 = 0x01;
const MSG_HELLO_ACK: u8 = 0x02;
const MSG_VIDEO_START: u8 = 0x03;
const MSG_VIDEO_STOP: u8 = 0x04;
const MSG_VIDEO_STATE: u8 = 0x05;
const MSG_VIDEO_FRAME: u8 = 0x06;
const MSG_ERROR: u8 = 0x07;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let socket = env::var("JETKVM_RDP_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_owned());
    let sample_limit = env::var("JETKVM_H264_PROBE_FRAMES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);

    let mut stream = UnixStream::connect(&socket).await?;
    write_message(&mut stream, MSG_HELLO, &PROTOCOL_VERSION.to_le_bytes()).await?;

    loop {
        let (typ, payload) = read_message(&mut stream).await?;
        match typ {
            MSG_HELLO_ACK => {
                if payload.len() != 2 || u16::from_le_bytes([payload[0], payload[1]]) != PROTOCOL_VERSION {
                    anyhow::bail!("bridge protocol mismatch");
                }
                println!("bridge handshake OK, requesting H.264 video");
                write_message(&mut stream, MSG_VIDEO_START, &[0]).await?;
                break;
            }
            MSG_ERROR => println!("bridge error before start: {}", String::from_utf8_lossy(&payload)),
            _ => {}
        }
    }

    let mut frames = 0usize;
    while frames < sample_limit {
        let result = timeout(Duration::from_secs(10), read_message(&mut stream)).await;
        let (typ, payload) = match result {
            Ok(result) => result?,
            Err(_) => {
                println!("timed out waiting for video frames");
                break;
            }
        };

        match typ {
            MSG_VIDEO_STATE => {
                if payload.len() >= 9 {
                    let ready = payload[0] != 0;
                    let width = u16::from_le_bytes([payload[1], payload[2]]);
                    let height = u16::from_le_bytes([payload[3], payload[4]]);
                    let fps_milli = u32::from_le_bytes(payload[5..9].try_into()?);
                    println!("video state: ready={ready} {width}x{height} {:.3} fps", fps_milli as f64 / 1000.0);
                }
            }
            MSG_VIDEO_FRAME => {
                if payload.len() < 9 {
                    println!("short VIDEO_FRAME: {} bytes", payload.len());
                    continue;
                }
                let duration_us = u32::from_le_bytes(payload[0..4].try_into()?);
                let width = u16::from_le_bytes(payload[4..6].try_into()?);
                let height = u16::from_le_bytes(payload[6..8].try_into()?);
                let codec = payload[8];
                let data = &payload[9..];
                let analysis = analyse_h264(data);
                frames += 1;
                let head = data.iter().take(16).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
                println!(
                    "frame #{frames}: codec={codec} {width}x{height} duration={duration_us}us bytes={} format={} nals={:?} sps={} pps={} idr={} head=[{}]",
                    data.len(), analysis.format, analysis.nal_types, analysis.has_sps, analysis.has_pps, analysis.has_idr, head
                );
            }
            MSG_ERROR => println!("bridge error: {}", String::from_utf8_lossy(&payload)),
            _ => {}
        }
    }

    let _ = write_message(&mut stream, MSG_VIDEO_STOP, &[]).await;
    Ok(())
}

#[derive(Debug)]
struct H264Analysis {
    format: &'static str,
    nal_types: Vec<u8>,
    has_sps: bool,
    has_pps: bool,
    has_idr: bool,
}

fn analyse_h264(data: &[u8]) -> H264Analysis {
    if let Some(nals) = annex_b_nal_types(data) {
        return summarise("annex-b", nals);
    }
    if let Some(nals) = avcc_nal_types(data) {
        return summarise("avcc-4byte", nals);
    }
    if let Some(first) = data.first() {
        let typ = first & 0x1f;
        if (1..=23).contains(&typ) {
            return summarise("raw-single-nal", vec![typ]);
        }
    }
    summarise("unknown", Vec::new())
}

fn summarise(format: &'static str, nal_types: Vec<u8>) -> H264Analysis {
    H264Analysis {
        format,
        has_sps: nal_types.contains(&7),
        has_pps: nal_types.contains(&8),
        has_idr: nal_types.contains(&5),
        nal_types,
    }
}

fn annex_b_nal_types(data: &[u8]) -> Option<Vec<u8>> {
    let starts = annex_b_starts(data);
    if starts.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (_, payload_start) in starts {
        if let Some(header) = data.get(payload_start) {
            out.push(header & 0x1f);
        }
    }
    Some(out)
}

fn annex_b_starts(data: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1] {
            starts.push((i, i + 4));
            i += 4;
        } else if data[i..i + 3] == [0, 0, 1] {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
}

fn avcc_nal_types(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 5 {
        return None;
    }
    let mut offset = 0usize;
    let mut nals = Vec::new();
    while offset + 4 <= data.len() {
        let len = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;
        if len == 0 || offset.checked_add(len)? > data.len() {
            return None;
        }
        let header = *data.get(offset)?;
        let typ = header & 0x1f;
        if !(1..=23).contains(&typ) {
            return None;
        }
        nals.push(typ);
        offset += len;
    }
    if offset == data.len() && !nals.is_empty() {
        Some(nals)
    } else {
        None
    }
}

async fn read_message(stream: &mut UnixStream) -> anyhow::Result<(u8, Vec<u8>)> {
    let typ = stream.read_u8().await?;
    let length = stream.read_u32_le().await? as usize;
    if length > MAX_MESSAGE {
        anyhow::bail!("bridge message too large: {length}");
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).await?;
    Ok((typ, payload))
}

async fn write_message(stream: &mut UnixStream, typ: u8, payload: &[u8]) -> io::Result<()> {
    stream.write_u8(typ).await?;
    stream.write_u32_le(payload.len() as u32).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}
