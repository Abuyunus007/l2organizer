use anyhow::{bail, Context, Result};

use crate::reader::Reader;

pub const PACKAGE_TAG: u32 = 0x9E2A83C1;

#[derive(Debug, Clone)]
pub struct Import {
    pub class_package: i32,
    pub class_name: i32,
    pub outer: i32,
    pub name: i32,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub class: i32,
    pub super_obj: i32,
    pub outer: i32,
    pub name: i32,
    pub flags: u32,
    pub serial_size: i32,
    pub serial_offset: i32,
}

pub struct Package {
    pub file_name: String,
    pub version: u16,
    pub licensee: u16,
    pub names: Vec<String>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub data: Vec<u8>,
}

impl Package {
    pub fn parse(file_name: String, data: Vec<u8>) -> Result<Self> {
        let mut r = Reader::new(&data);

        let tag = r.u32()?;
        if tag != PACKAGE_TAG {
            bail!("bad package tag {tag:#010X}");
        }
        let version = r.u16()?;
        let licensee = r.u16()?;
        let _flags = r.u32()?;
        let name_count = r.i32()?;
        let name_offset = r.i32()?;
        let export_count = r.i32()?;
        let export_offset = r.i32()?;
        let import_count = r.i32()?;
        let import_offset = r.i32()?;

        for (label, count, offset) in [
            ("names", name_count, name_offset),
            ("exports", export_count, export_offset),
            ("imports", import_count, import_offset),
        ] {
            if !(0..=2_000_000).contains(&count) {
                bail!("implausible {label} count {count}");
            }
            if offset < 0 || offset as usize > data.len() {
                bail!("implausible {label} offset {offset:#X}");
            }
        }

        if version >= 68 {
            r.skip(16)?;
            let generations = r.i32()?;
            if !(0..=64).contains(&generations) {
                bail!("implausible generation count {generations}");
            }
            r.skip(generations as usize * 8)?;
        } else {
            r.skip(8)?;
        }

        if std::env::var("L2O_DEBUG").is_ok() {
            eprintln!(
                "hdr: version={version} licensee={licensee} names={name_count}@{name_offset:#X} imports={import_count}@{import_offset:#X} exports={export_count}@{export_offset:#X} pos={:#X}",
                r.pos()
            );
            let dump_start = name_offset as usize;
            let dump_end = (dump_start + 48).min(data.len());
            let hex: Vec<String> = data[dump_start..dump_end]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            eprintln!("names[0..48]: {}", hex.join(" "));
        }

        r.seek(name_offset as usize)?;
        let mut names = Vec::with_capacity(name_count as usize);
        for i in 0..name_count {
            names.push(
                r.fstring()
                    .with_context(|| format!("name #{i} at {:#X}", r.pos()))?,
            );
            let _flags = r.u32().with_context(|| format!("flags of name #{i}"))?;
        }

        r.seek(import_offset as usize)?;
        let mut imports = Vec::with_capacity(import_count as usize);
        for i in 0..import_count {
            imports.push(Import {
                class_package: r.compact().with_context(|| format!("import #{i}"))?,
                class_name: r.compact()?,
                outer: r.i32()?,
                name: r.compact()?,
            });
        }

        r.seek(export_offset as usize)?;
        let mut exports = Vec::with_capacity(export_count as usize);
        for i in 0..export_count {
            let class = r.compact().with_context(|| format!("export #{i}"))?;
            let super_obj = r.compact()?;
            let outer = r.i32()?;
            let name = r.compact()?;
            let flags = r.u32()?;
            let serial_size = r.compact()?;
            let serial_offset = if serial_size != 0 { r.compact()? } else { 0 };
            exports.push(Export {
                class,
                super_obj,
                outer,
                name,
                flags,
                serial_size,
                serial_offset,
            });
        }

        Ok(Self {
            file_name,
            version,
            licensee,
            names,
            imports,
            exports,
            data,
        })
    }

    pub fn name_at(&self, idx: i32) -> Option<&str> {
        if idx >= 0 {
            self.names.get(idx as usize).map(String::as_str)
        } else {
            let imp = self.imports.get((-idx - 1) as usize)?;
            if imp.name < 0 {
                return None;
            }
            self.name_at(imp.name)
        }
    }

    pub fn export_name(&self, e: &Export) -> String {
        self.name_at(e.name).unwrap_or_default().to_string()
    }

    pub fn export_class(&self, e: &Export) -> String {
        self.name_at(e.class).unwrap_or_default().to_string()
    }

    pub fn export_group(&self, e: &Export) -> Option<String> {
        if e.outer > 0 {
            self.exports
                .get((e.outer - 1) as usize)
                .map(|g| self.export_name(g))
        } else {
            None
        }
    }

    pub fn export_data(&self, e: &Export) -> Result<&[u8]> {
        if e.serial_size <= 0 || e.serial_offset < 0 {
            bail!("export has no serial data");
        }
        let start = e.serial_offset as usize;
        let end = start
            .checked_add(e.serial_size as usize)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| anyhow::anyhow!("export data range out of bounds"))?;
        Ok(&self.data[start..end])
    }
}
