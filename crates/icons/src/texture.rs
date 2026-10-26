use anyhow::{bail, Result};

use crate::package::Package;
use crate::reader::Reader;

pub struct Mip {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct Texture {
    pub format: u8,
    pub palette: i32,
    pub mips: Vec<Mip>,
}

pub struct PropertyScan {
    pub format: Option<u8>,
    pub palette: Option<i32>,
}

const PROP_BYTE: u8 = 1;
const PROP_INT: u8 = 2;
const PROP_BOOL: u8 = 3;
const PROP_OBJECT: u8 = 5;
const PROP_STRUCT: u8 = 10;

pub fn read_fname(r: &mut Reader, pkg: &Package) -> Result<String> {
    let idx = r.compact()?;
    Ok(pkg
        .name_at(idx)
        .map(str::to_string)
        .unwrap_or_else(|| format!("<bad-name:{idx}>")))
}

pub fn skip_properties(r: &mut Reader<'_>, pkg: &Package) -> Result<PropertyScan> {
    let mut scan = PropertyScan {
        format: None,
        palette: None,
    };
    loop {
        let name = read_fname(r, pkg)?;
        if name == "None" {
            break;
        }
        let info = r.u8()?;
        let is_array = info & 0x80 != 0;
        let ty = info & 0x0F;
        if ty == PROP_STRUCT {
            let _struc = read_fname(r, pkg)?;
        }
        let size = match (info >> 4) & 7 {
            0 => 1usize,
            1 => 2,
            2 => 4,
            3 => 12,
            4 => 16,
            5 => r.u8()? as usize,
            6 => r.u16()? as usize,
            7 => r.i32()?.max(0) as usize,
            _ => unreachable!(),
        };
        if ty != PROP_BOOL && is_array {
            let b = r.u8()? as u32;
            if b >= 128 {
                let _b2 = r.u8()?;
                if b & 0x40 != 0 {
                    let _b3 = r.u8()?;
                    let _b4 = r.u8()?;
                }
            }
        }
        if ty == PROP_BOOL {
            continue;
        }
        let value_start = r.pos();
        if name == "Format" && ty == PROP_BYTE && size >= 1 {
            scan.format = Some(r.u8()?);
        } else if name == "Palette" && ty == PROP_OBJECT && size >= 1 {
            scan.palette = Some(r.compact()?);
        }
        r.seek(value_start + size)?;
    }
    Ok(scan)
}

pub fn skip_lineage_material(r: &mut Reader<'_>, pkg: &Package) -> Result<()> {
    let ver = pkg.version;
    let lic = pkg.licensee;
    if ver < 123 {
        return Ok(());
    }
    if (16..37).contains(&lic) {
        r.skip(4)?;
    }
    if (30..37).contains(&lic) {
        if (33..36).contains(&lic) {
            r.skip(1)?;
        }
        r.skip(6)?;
        r.skip(12)?;
        for _ in 0..8 {
            r.skip(1)?;
            if lic < 36 {
                r.skip(1)?;
            }
            r.skip(126)?;
        }
        r.skip(8)?;
        r.skip(12)?;
        for _ in 0..16 {
            let _ = r.fstring()?;
        }
        let _ = r.fstring()?;
    }
    if lic >= 37 {
        r.skip(2)?;
        if ver < 129 {
            r.skip(14)?;
        } else {
            r.skip(70)?;
        }
        r.skip(8)?;
        r.skip(12)?;
        let stages = r.compact()?;
        if !(0..=64).contains(&stages) {
            bail!("implausible material stage count {stages}");
        }
        for _ in 0..stages {
            let _ = r.fstring()?;
            let n = r.compact()?;
            if !(0..=64).contains(&n) {
                bail!("implausible stage texture count {n}");
            }
            for _ in 0..n {
                let _ = r.fstring()?;
            }
        }
        let _ = r.fstring()?;
    }
    if lic >= 31 {
        r.skip(4)?;
    }
    Ok(())
}

pub fn parse_export(pkg: &Package, export_index: usize) -> Result<Texture> {
    let e = &pkg.exports[export_index];
    let data = pkg.export_data(e)?;

    if std::env::var("L2O_DEBUG").is_ok() {
        let dump_end = data.len().min(160);
        let hex: Vec<String> = data[..dump_end].iter().map(|b| format!("{b:02X}")).collect();
        let tail_start = data.len().saturating_sub(160);
        let tail: Vec<String> = data[tail_start..].iter().map(|b| format!("{b:02X}")).collect();
        let nonzero: Vec<usize> = data
            .iter()
            .enumerate()
            .filter(|(_, b)| **b != 0)
            .map(|(i, _)| i)
            .take(40)
            .collect();
        eprintln!(
            "  export {export_index} size={} first160: {}",
            data.len(),
            hex.join(" ")
        );
        eprintln!("  tail160: {}", tail.join(" "));
        eprintln!("  first nonzero offsets: {nonzero:?}");
        let ascii: String = data
            .iter()
            .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
            .collect();
        eprintln!("  ascii: {ascii}");
    }

    let mut r = Reader::new(data);
    let scan = skip_properties(&mut r, pkg)?;
    skip_lineage_material(&mut r, pkg)?;

    if std::env::var("L2O_DEBUG").is_ok() {
        let look = (r.pos() + 12).min(data.len());
        let hex: Vec<String> = data[r.pos()..look]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        eprintln!(
            "  props done at +{:#X}: format={:?} palette={:?} next: {}",
            r.pos(),
            scan.format,
            scan.palette,
            hex.join(" ")
        );
    }

    let mip_count = r.compact()?;
    if !(1..=16).contains(&mip_count) {
        bail!("implausible mip count {mip_count}");
    }
    let mut mips = Vec::with_capacity(mip_count as usize);
    for _ in 0..mip_count {
        let _skip_pos = r.i32()?;
        let data_len = r.compact()?;
        if !(0..=64 * 1024 * 1024).contains(&data_len) {
            bail!("implausible mip data size {data_len}");
        }
        let data = r.bytes(data_len as usize)?.to_vec();
        let width = r.i32()?;
        let height = r.i32()?;
        let _ubits = r.u8()?;
        let _vbits = r.u8()?;
        if !(0..=8192).contains(&width) || !(0..=8192).contains(&height) {
            bail!("implausible mip dimensions {width}x{height}");
        }
        mips.push(Mip {
            width: width as u32,
            height: height as u32,
            data,
        });
    }

    Ok(Texture {
        format: scan.format.unwrap_or(0xFF),
        palette: scan.palette.unwrap_or(0),
        mips,
    })
}

pub fn parse_palette(pkg: &Package, export_index: usize) -> Result<Vec<[u8; 4]>> {
    let e = &pkg.exports[export_index];
    let data = pkg.export_data(e)?;
    let mut r = Reader::new(data);
    let _ = skip_properties(&mut r, pkg)?;
    let count = r.compact()?;
    if !(1..=256).contains(&count) {
        bail!("implausible palette size {count}");
    }
    let mut colors = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let b = r.bytes(4)?;
        colors.push([b[0], b[1], b[2], b[3]]);
    }
    Ok(colors)
}
