use anyhow::{bail, Result};

const HEADER_SIZE: usize = 28;
const TAIL_SIZE: usize = 20;

pub fn is_encrypted(data: &[u8]) -> bool {
    data.len() >= HEADER_SIZE + TAIL_SIZE
        && data[0] == b'L'
        && data[1] == 0
        && data[2] == b'i'
        && data[3] == 0
}

pub fn protocol(data: &[u8]) -> Option<u32> {
    if !is_encrypted(data) {
        return None;
    }
    let d0 = data[22];
    let d1 = data[24];
    let d2 = data[26];
    if !d0.is_ascii_digit() || !d1.is_ascii_digit() || !d2.is_ascii_digit() {
        return None;
    }
    Some((d0 - b'0') as u32 * 100 + (d1 - b'0') as u32 * 10 + (d2 - b'0') as u32)
}

fn key_by_index(index: u32) -> u8 {
    let d1 = index & 0xF;
    let d2 = (index >> 4) & 0xF;
    let d3 = (index >> 8) & 0xF;
    let d4 = (index >> 12) & 0xF;
    ((((d2 ^ d4) << 4) | (d1 ^ d3)) & 0xFF) as u8
}

fn key_by_filename(file_name: &str) -> u8 {
    file_name
        .to_ascii_lowercase()
        .bytes()
        .fold(0u8, |acc, b| acc.wrapping_add(b))
}

pub fn decrypt(data: &[u8], file_name: &str) -> Result<Vec<u8>> {
    if !is_encrypted(data) {
        return Ok(data.to_vec());
    }
    let proto = protocol(data).unwrap_or(0);
    let payload = &data[HEADER_SIZE..data.len() - TAIL_SIZE];
    if payload.is_empty() {
        bail!("encrypted file has empty payload");
    }
    let mut out = payload.to_vec();

    match proto {
        111 => {
            let key = 0xAC;
            for b in out.iter_mut() {
                *b ^= key;
            }
        }
        120 => {
            let mut index: u32 = 0xE6;
            for b in out.iter_mut() {
                *b ^= key_by_index(index);
                index = index.wrapping_add(1);
            }
        }
        121 => {
            let key = payload[0] ^ 0xC1;
            let base = std::path::Path::new(file_name)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name.to_string());
            let file_key = key_by_filename(&base);
            if file_key != key {
                eprintln!(
                    "note: {file_name}: filename-derived key {file_key:#04X} != header-derived key {key:#04X}, using header-derived"
                );
            }
            for b in out.iter_mut() {
                *b ^= key;
            }
        }
        other => bail!("unsupported Lineage2 crypt protocol {other}"),
    }

    if out.len() >= 4 {
        let tag = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        if tag != crate::package::PACKAGE_TAG {
            bail!("decrypted data does not start with a package tag ({tag:#010X})");
        }
    }

    Ok(out)
}
