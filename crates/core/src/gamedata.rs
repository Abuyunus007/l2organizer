use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ItemInfo {
    pub id: i32,
    pub name: String,
    pub icon: String,
    pub stackable: bool,
    pub kind: String,
    pub crystal_type: Option<String>,
    pub crystal_count: i64,
    pub price: i64,
}

#[derive(Debug, Clone)]
pub struct Recipe {
    pub id: i32,
    pub name: String,
    pub level: i32,
    pub dwarven: bool,
    pub success_rate: i32,
    pub mp_cost: i64,
    pub hp_cost: i64,
    pub ingredients: Vec<(i32, i64)>,
    pub product_id: i32,
    pub product_count: i64,
    pub rare_item_id: Option<i32>,
    pub rare_count: i64,
    pub rarity: i64,
}

pub fn load_recipes(file: &Path) -> Result<HashMap<i32, Recipe>> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let doc = roxmltree::Document::parse(&text)
        .with_context(|| format!("parsing {}", file.display()))?;
    let mut out = HashMap::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let Some(id) = node.attribute("id").and_then(|v| v.parse::<i32>().ok()) else {
            continue;
        };
        let name = node.attribute("name").unwrap_or_default().to_string();
        let level = node
            .attribute("craftLevel")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        let dwarven = node
            .attribute("type")
            .is_some_and(|v| v.eq_ignore_ascii_case("dwarven"));
        let success_rate = node
            .attribute("successRate")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(100);
        let mut mp_cost = 0i64;
        let mut hp_cost = 0i64;
        let mut ingredients = Vec::new();
        let mut product_id = 0i32;
        let mut product_count = 1i64;
        let mut rare_item_id = None;
        let mut rare_count = 0i64;
        let mut rarity = 0i64;
        for child in node.children() {
            match child.tag_name().name() {
                "ingredient" => {
                    if let (Some(iid), Some(count)) = (
                        child.attribute("id").and_then(|v| v.parse::<i32>().ok()),
                        child.attribute("count").and_then(|v| v.parse::<i64>().ok()),
                    ) {
                        ingredients.push((iid, count));
                    }
                }
                "production" => {
                    product_id = child
                        .attribute("id")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    product_count = child
                        .attribute("count")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1);
                }
                "productionRare" => {
                    rare_item_id = child.attribute("id").and_then(|v| v.parse().ok());
                    rare_count = child
                        .attribute("count")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1);
                    rarity = child
                        .attribute("rarity")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                }
                "statUse" => {
                    let value = child
                        .attribute("value")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0);
                    match child.attribute("name") {
                        Some("MP") => mp_cost += value,
                        Some("HP") => hp_cost += value,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if product_id == 0 || ingredients.is_empty() {
            continue;
        }
        out.insert(
            id,
            Recipe {
                id,
                name,
                level,
                dwarven,
                success_rate,
                mp_cost,
                hp_cost,
                ingredients,
                product_id,
                product_count,
                rare_item_id,
                rare_count,
                rarity,
            },
        );
    }
    Ok(out)
}

pub fn load_items(dir: &Path) -> Result<HashMap<i32, ItemInfo>> {
    let mut files = Vec::new();
    collect_xml(dir, &mut files)?;
    let mut out = HashMap::new();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let doc = roxmltree::Document::parse(&text)
            .with_context(|| format!("parsing {}", file.display()))?;
        for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
            let Some(id) = item.attribute("id").and_then(|v| v.parse::<i32>().ok()) else {
                continue;
            };
            let name = item.attribute("name").unwrap_or_default().to_string();
            let kind = item.attribute("type").unwrap_or_default().to_string();
            let settings = item.children().filter(|c| c.has_tag_name("set"));
            let icon = settings
                .clone()
                .find(|c| c.attribute("name") == Some("icon"))
                .and_then(|c| c.attribute("val"))
                .unwrap_or_default()
                .to_string();
            let stackable = settings
                .clone()
                .filter(|c| c.attribute("name") == Some("is_stackable"))
                .any(|c| c.attribute("val") == Some("true"));
            let crystal_type = settings
                .clone()
                .find(|c| c.attribute("name") == Some("crystal_type"))
                .and_then(|c| c.attribute("val"))
                .map(|v| v.to_uppercase());
            let crystal_count = settings
                .clone()
                .filter(|c| c.attribute("name") == Some("crystal_count"))
                .find_map(|c| c.attribute("val"))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let price = settings
                .filter(|c| c.attribute("name") == Some("price"))
                .find_map(|c| c.attribute("val"))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            out.entry(id).or_insert(ItemInfo {
                id,
                name,
                icon,
                stackable,
                kind,
                crystal_type,
                crystal_count,
                price,
            });
        }
    }
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
