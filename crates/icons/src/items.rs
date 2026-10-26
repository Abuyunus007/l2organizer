use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn load_icons(dir: &Path) -> Result<Vec<(i32, String)>> {
    let mut files = Vec::new();
    collect_xml(dir, &mut files)?;
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let doc = roxmltree::Document::parse(&text)
            .with_context(|| format!("parsing {}", file.display()))?;
        for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
            let Some(id) = item.attribute("id").and_then(|v| v.parse::<i32>().ok()) else {
                continue;
            };
            let icon = item
                .children()
                .filter(|c| c.has_tag_name("set"))
                .find(|c| c.attribute("name") == Some("icon"))
                .and_then(|c| c.attribute("val"));
            if let Some(icon) = icon {
                out.push((id, icon.to_string()));
            }
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out.dedup_by_key(|(id, _)| *id);
    Ok(out)
}

fn collect_xml(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_xml(&path, files)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")) {
            files.push(path);
        }
    }
    Ok(())
}
