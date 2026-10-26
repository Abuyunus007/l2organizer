mod items;
mod l2crypt;
mod package;
mod pixel;
mod reader;
mod texture;

use anyhow::{bail, Context, Result};
use package::Package;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("extract") => cmd_extract(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        _ => {
            eprintln!("usage:");
            eprintln!("  l2o-icons extract --items <dir> --textures <dir> --out <dir> [--limit N]");
            eprintln!("  l2o-icons inspect <file.utx> [name-filter] [--raw] [--dump <name> <out.png>]");
            Ok(())
        }
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn load_package(path: &Path) -> Result<Package> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = l2crypt::decrypt(&raw, &name)?;
    Package::parse(name, data).with_context(|| format!("parsing {}", path.display()))
}

fn cmd_extract(args: &[String]) -> Result<()> {
    let Some(items_dir) = arg_value(args, "--items") else {
        bail!("--items <dir> is required");
    };
    let Some(tex_dir) = arg_value(args, "--textures") else {
        bail!("--textures <dir> is required");
    };
    let Some(out_dir) = arg_value(args, "--out") else {
        bail!("--out <dir> is required");
    };
    let limit = arg_value(args, "--limit").and_then(|v| v.parse::<usize>().ok());

    let icons = items::load_icons(Path::new(&items_dir))?;
    println!("items with icons: {}", icons.len());

    let mut packages: Vec<Package> = Vec::new();
    for entry in std::fs::read_dir(&tex_dir).with_context(|| format!("reading {tex_dir}"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("utx"))
        {
            match load_package(&path) {
                Ok(pkg) => packages.push(pkg),
                Err(e) => eprintln!("skip {}: {e:#}", path.display()),
            }
        }
    }
    println!("packages loaded: {}", packages.len());

    let mut index: HashMap<String, (usize, usize)> = HashMap::new();
    for (pi, pkg) in packages.iter().enumerate() {
        let stem = Path::new(&pkg.file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        for (ei, e) in pkg.exports.iter().enumerate() {
            if pkg.export_class(e) != "Texture" {
                continue;
            }
            let name = pkg.export_name(e);
            if let Some(group) = pkg.export_group(e) {
                if !group.is_empty() && group != "None" {
                    index
                        .entry(format!("{group}.{name}"))
                        .or_insert((pi, ei));
                }
            }
            if !stem.is_empty() {
                index
                    .entry(format!("{stem}.{name}"))
                    .or_insert((pi, ei));
            }
            index.entry(name).or_insert((pi, ei));
        }
    }
    println!("textures indexed: {}", index.len());

    let lookup = |icon: &str| -> Option<(usize, usize)> {
        if let Some(loc) = index.get(icon) {
            return Some(*loc);
        }
        let lower = icon.to_lowercase();
        if let Some(loc) = index.get(&lower) {
            return Some(*loc);
        }
        if let Some((_, rest)) = icon.split_once('.') {
            let lower_rest = rest.to_lowercase();
            if let Some(loc) = index.get(rest).or_else(|| index.get(&lower_rest)) {
                return Some(*loc);
            }
        }
        None
    };

    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {out_dir}"))?;
    let mut cache: HashMap<String, Option<(Vec<u8>, u32, u32)>> = HashMap::new();
    let mut missing: Vec<(i32, String, String)> = Vec::new();
    let mut written = 0usize;
    let mut seen = 0usize;

    for (id, icon) in &icons {
        if let Some(limit) = limit {
            if seen >= limit {
                break;
            }
        }
        seen += 1;
        let Some((pi, ei)) = lookup(icon) else {
            missing.push((*id, icon.clone(), "texture not found".into()));
            continue;
        };
        let entry = cache.entry(icon.clone()).or_insert_with(|| {
            decode_texture(&packages[pi], ei)
                .map_err(|e| eprintln!("decode {icon}: {e:#}"))
                .ok()
        });
        let Some((rgba, w, h)) = entry else {
            missing.push((*id, icon.clone(), "decode failed".into()));
            continue;
        };
        let img = image::RgbaImage::from_raw(*w, *h, rgba.clone())
            .ok_or_else(|| anyhow::anyhow!("bad image dimensions for {icon}"))?;
        let path = Path::new(&out_dir).join(format!("{id}.png"));
        img.save_with_format(&path, image::ImageFormat::Png)
            .with_context(|| format!("saving {}", path.display()))?;
        written += 1;
        if seen % 1000 == 0 {
            println!("  ... {seen} items processed");
        }
    }

    let missing_path = Path::new(&out_dir).join("_missing.txt");
    let mut report = String::new();
    for (id, icon, reason) in &missing {
        report.push_str(&format!("{id}\t{icon}\t{reason}\n"));
    }
    std::fs::write(&missing_path, &report)?;

    println!("unique textures decoded: {}", cache.len());
    println!("pngs written: {written}");
    println!("missing: {} (see {})", missing.len(), missing_path.display());
    Ok(())
}

fn decode_texture(pkg: &Package, export_index: usize) -> Result<(Vec<u8>, u32, u32)> {
    let tex = texture::parse_export(pkg, export_index)?;
    let mip = tex
        .mips
        .first()
        .ok_or_else(|| anyhow::anyhow!("texture has no mips"))?;
    let palette = if tex.format == 0x00 && tex.palette > 0 {
        let idx = (tex.palette - 1) as usize;
        if idx < pkg.exports.len() {
            Some(texture::parse_palette(pkg, idx)?)
        } else {
            None
        }
    } else {
        None
    };
    let rgba = pixel::decode(
        tex.format,
        &mip.data,
        mip.width as usize,
        mip.height as usize,
        palette.as_deref(),
    )?;
    Ok((rgba, mip.width, mip.height))
}

fn cmd_inspect(args: &[String]) -> Result<()> {
    let Some(path) = args.first() else {
        bail!("file.utx required");
    };
    let filter = args
        .get(1)
        .filter(|a| !a.starts_with("--"))
        .cloned();
    let raw = std::fs::read(path)?;
    println!("file: {path} ({} bytes)", raw.len());
    if l2crypt::is_encrypted(&raw) {
        println!(
            "L2 crypt protocol: {:?}",
            l2crypt::protocol(&raw).unwrap_or(0)
        );
    }
    let file_name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = l2crypt::decrypt(&raw, &file_name)?;
    let pkg = Package::parse(file_name.clone(), data)?;
    println!(
        "version: {} licensee: {} names: {} imports: {} exports: {}",
        pkg.version,
        pkg.licensee,
        pkg.names.len(),
        pkg.imports.len(),
        pkg.exports.len()
    );
    println!("first names: {:?}", &pkg.names[..pkg.names.len().min(12)]);

    let mut class_counts: HashMap<String, usize> = HashMap::new();
    for e in &pkg.exports {
        *class_counts.entry(pkg.export_class(e)).or_default() += 1;
    }
    let mut classes: Vec<_> = class_counts.into_iter().collect();
    classes.sort_by(|a, b| b.1.cmp(&a.1));
    println!("export classes: {:?}", &classes[..classes.len().min(15)]);

    if let Some(filter) = filter {
        let mut shown = 0;
        for (ei, e) in pkg.exports.iter().enumerate() {
            if pkg.export_class(e) != "Texture" {
                continue;
            }
            let name = pkg.export_name(e);
            let group = pkg.export_group(e).unwrap_or_default();
            let full = if group.is_empty() || group == "None" {
                name.clone()
            } else {
                format!("{group}.{name}")
            };
            if !full.contains(filter.as_str()) {
                continue;
            }
            match texture::parse_export(&pkg, ei) {
                Ok(tex) => {
                    println!(
                        "#{ei} {full}: format={:#04X} palette={} mips={:?}",
                        tex.format,
                        tex.palette,
                        tex.mips
                            .iter()
                            .map(|m| format!("{}x{} ({}b)", m.width, m.height, m.data.len()))
                            .collect::<Vec<_>>()
                    );
                }
                Err(err) => println!("#{ei} {full}: parse error: {err:#}"),
            }
            shown += 1;
            if shown >= 50 {
                break;
            }
        }
        if shown == 0 {
            println!("no texture matched filter {filter:?}");
        }
    }

    if let (Some(dump_name), Some(out_path)) =
        (arg_value(args, "--dump"), arg_value(args, "--out"))
    {
        if let Some(ei) = pkg.exports.iter().enumerate().find_map(|(ei, e)| {
            if pkg.export_class(e) != "Texture" {
                return None;
            }
            let name = pkg.export_name(e);
            let group = pkg.export_group(e).unwrap_or_default();
            let full = if group.is_empty() || group == "None" {
                name
            } else {
                format!("{group}.{name}")
            };
            (full == dump_name).then_some(ei)
        }) {
            let (rgba, w, h) = decode_texture(&pkg, ei)?;
            let img = image::RgbaImage::from_raw(w, h, rgba).unwrap();
            img.save_with_format(PathBuf::from(&out_path), image::ImageFormat::Png)?;
            println!("dumped {dump_name} -> {out_path} ({w}x{h})");
        } else {
            println!("dump target {dump_name:?} not found");
        }
    }

    Ok(())
}
