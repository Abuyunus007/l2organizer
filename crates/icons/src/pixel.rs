use anyhow::{bail, Result};
use std::io::Read;

pub fn expected_raw_size(format: u8, w: usize, h: usize) -> Option<usize> {
    let blocks = w.div_ceil(4) * h.div_ceil(4);
    Some(match format {
        0x00 | 0x09 => w * h,
        0x01 | 0x02 | 0x0A => w * h * 2,
        0x03 => blocks * 8,
        0x04 => w * h * 3,
        0x05 => w * h * 4,
        0x07 | 0x08 => blocks * 16,
        0x0B => w * h * 4,
        _ => return None,
    })
}

fn prepare_data(data: &[u8], expected: usize) -> Vec<u8> {
    if data.len() == expected {
        return data.to_vec();
    }
    if data.len() < expected && data.len() >= 2 && data[0] == 0x78 {
        let mut decoded = Vec::with_capacity(expected);
        if flate2::read::ZlibDecoder::new(data)
            .read_to_end(&mut decoded)
            .is_ok()
            && decoded.len() >= expected
        {
            decoded.truncate(expected);
            return decoded;
        }
    }
    let mut out = data.to_vec();
    if out.len() > expected {
        out.truncate(expected);
    } else {
        out.resize(expected, 0);
    }
    out
}

pub fn decode(
    format: u8,
    data: &[u8],
    w: usize,
    h: usize,
    palette: Option<&[[u8; 4]]>,
) -> Result<Vec<u8>> {
    if w == 0 || h == 0 {
        bail!("zero-sized texture");
    }
    let expected = expected_raw_size(format, w, h);
    if expected.is_none() {
        bail!("unsupported texture format {format}");
    }
    let raw = prepare_data(data, expected.unwrap());
    let mut out = vec![0u8; w * h * 4];

    match format {
        0x00 => {
            let palette = palette.ok_or_else(|| anyhow::anyhow!("P8 texture without palette"))?;
            for i in 0..w * h {
                let idx = raw.get(i).copied().unwrap_or(0) as usize;
                let c = palette.get(idx).copied().unwrap_or([0, 0, 0, 255]);
                out[i * 4..i * 4 + 4].copy_from_slice(&c);
            }
        }
        0x03 => decode_dxt(&raw, w, h, DxtKind::Bc1, &mut out),
        0x07 => decode_dxt(&raw, w, h, DxtKind::Bc2, &mut out),
        0x08 => decode_dxt(&raw, w, h, DxtKind::Bc3, &mut out),
        0x04 => {
            for i in 0..w * h {
                let s = &raw[i * 3..i * 3 + 3];
                out[i * 4] = s[2];
                out[i * 4 + 1] = s[1];
                out[i * 4 + 2] = s[0];
                out[i * 4 + 3] = 255;
            }
        }
        0x05 => {
            for i in 0..w * h {
                let s = &raw[i * 4..i * 4 + 4];
                out[i * 4] = s[2];
                out[i * 4 + 1] = s[1];
                out[i * 4 + 2] = s[0];
                out[i * 4 + 3] = s[3];
            }
        }
        0x09 => {
            for i in 0..w * h {
                let b = raw[i];
                out[i * 4] = b;
                out[i * 4 + 1] = b;
                out[i * 4 + 2] = b;
                out[i * 4 + 3] = 255;
            }
        }
        other => bail!("unsupported texture format {other}"),
    }

    Ok(out)
}

#[derive(Clone, Copy, PartialEq)]
enum DxtKind {
    Bc1,
    Bc2,
    Bc3,
}

fn expand565(c: u16) -> (u8, u8, u8) {
    let r = ((c >> 11) & 0x1F) as u8;
    let g = ((c >> 5) & 0x3F) as u8;
    let b = (c & 0x1F) as u8;
    (
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    )
}

fn color_table(c0: u16, c1: u16, bc1: bool) -> [[u8; 4]; 4] {
    let a = expand565(c0);
    let b = expand565(c1);
    let mut cols = [[0u8; 4]; 4];
    cols[0] = [a.0, a.1, a.2, 255];
    cols[1] = [b.0, b.1, b.2, 255];
    let mix = |x: u8, y: u8, wx: u16, wy: u16, div: u16| {
        ((x as u16 * wx + y as u16 * wy) / div) as u8
    };
    if !bc1 || c0 > c1 {
        cols[2] = [mix(a.0, b.0, 2, 1, 3), mix(a.1, b.1, 2, 1, 3), mix(a.2, b.2, 2, 1, 3), 255];
        cols[3] = [mix(a.0, b.0, 1, 2, 3), mix(a.1, b.1, 1, 2, 3), mix(a.2, b.2, 1, 2, 3), 255];
    } else {
        cols[2] = [mix(a.0, b.0, 1, 1, 2), mix(a.1, b.1, 1, 1, 2), mix(a.2, b.2, 1, 1, 2), 255];
        cols[3] = [0, 0, 0, 0];
    }
    cols
}

fn decode_block(block: &[u8], kind: DxtKind, tile: &mut [u8; 64]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let bc1 = kind == DxtKind::Bc1;
    let cols = color_table(c0, c1, bc1);
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

    let mut alphas = [255u8; 16];
    match kind {
        DxtKind::Bc1 => {
            for i in 0..16 {
                let idx = ((bits >> (2 * i)) & 3) as usize;
                tile[i * 4..i * 4 + 4].copy_from_slice(&cols[idx]);
            }
            return;
        }
        DxtKind::Bc2 => {
            for i in 0..16 {
                let byte = block[i / 2];
                let nib = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                alphas[i] = nib * 17;
            }
        }
        DxtKind::Bc3 => {
            let a0 = block[0] as u16;
            let a1 = block[1] as u16;
            let abits = (block[2] as u64)
                | (block[3] as u64) << 8
                | (block[4] as u64) << 16
                | (block[5] as u64) << 24
                | (block[6] as u64) << 32
                | (block[7] as u64) << 40;
            let mut table = [0u8; 8];
            table[0] = a0 as u8;
            table[1] = a1 as u8;
            if a0 > a1 {
                for i in 2..8usize {
                    let w0 = (8 - i) as u16;
                    let w1 = (i - 1) as u16;
                    table[i] = ((w0 * a0 + w1 * a1) / 7) as u8;
                }
            } else {
                for i in 2..6usize {
                    let w0 = (6 - i) as u16;
                    let w1 = (i - 1) as u16;
                    table[i] = ((w0 * a0 + w1 * a1) / 5) as u8;
                }
                table[6] = 0;
                table[7] = 255;
            }
            for i in 0..16 {
                let idx = ((abits >> (3 * i)) & 7) as usize;
                alphas[i] = table[idx];
            }
        }
    }

    for i in 0..16 {
        let idx = ((bits >> (2 * i)) & 3) as usize;
        let mut c = cols[idx];
        c[3] = alphas[i];
        tile[i * 4..i * 4 + 4].copy_from_slice(&c);
    }
}

fn decode_dxt(data: &[u8], w: usize, h: usize, kind: DxtKind, out: &mut [u8]) {
    let bw = w.div_ceil(4);
    let block_size = if kind == DxtKind::Bc1 { 8 } else { 16 };
    let mut tile = [0u8; 64];
    for by in 0..h.div_ceil(4) {
        for bx in 0..bw {
            let off = (by * bw + bx) * block_size;
            let Some(block) = data.get(off..off + block_size) else {
                continue;
            };
            decode_block(block, kind, &mut tile);
            for y in 0..4 {
                let py = by * 4 + y;
                if py >= h {
                    break;
                }
                for x in 0..4 {
                    let px = bx * 4 + x;
                    if px >= w {
                        continue;
                    }
                    let si = (y * 4 + x) * 4;
                    let di = (py * w + px) * 4;
                    out[di..di + 4].copy_from_slice(&tile[si..si + 4]);
                }
            }
        }
    }
}
