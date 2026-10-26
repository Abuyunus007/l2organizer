use anyhow::{bail, Result};

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            bail!("seek out of bounds: {pos:#X} > {:#X}", self.data.len());
        }
        self.pos = pos;
        Ok(())
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.seek(self.pos + n)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let Some(end) = end else {
            bail!("read out of bounds at {:#X} (want {n} bytes)", self.pos);
        };
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn compact(&mut self) -> Result<i32> {
        let b = self.u8()?;
        let negative = b & 0x80 != 0;
        let mut value: i64 = (b & 0x3F) as i64;
        let mut shift = 6u32;
        if b & 0x40 != 0 {
            loop {
                let next = self.u8()?;
                value |= ((next & 0x7F) as i64) << shift;
                shift += 7;
                if next & 0x80 == 0 {
                    break;
                }
                if shift > 35 {
                    bail!("compact index too long");
                }
            }
        }
        let value = if negative { -value } else { value };
        i32::try_from(value).map_err(|_| anyhow::anyhow!("compact index overflow: {value}"))
    }

    pub fn fstring(&mut self) -> Result<String> {
        let len = self.compact()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let bytes = self.bytes(len as usize)?;
            let s = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
            Ok(String::from_utf8_lossy(s).into_owned())
        } else {
            let n = len.unsigned_abs() as usize;
            let bytes = self.bytes(n * 2)?;
            let mut units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            while units.last() == Some(&0) {
                units.pop();
            }
            Ok(String::from_utf16_lossy(&units))
        }
    }
}
