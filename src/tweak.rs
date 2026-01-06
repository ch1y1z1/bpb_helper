use crate::pck;
use anyhow::{Context, Result, anyhow};
use rust_embed::RustEmbed;
use std::fs::OpenOptions;

#[derive(RustEmbed)]
#[folder = "assets"]
struct Assets;

/// 读取 replace.toml，解析替换与删除列表
fn load_config() -> Result<(Vec<(String, Vec<u8>)>, Vec<String>)> {
    let config_str = include_str!("../assets/replace.toml");
    let table: toml::value::Table = toml::from_str(config_str).context("解析 replace.toml 失败")?;

    // 读取 [replace] 表
    let replace_table = table
        .get("replace")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow!("replace.toml 缺少 [replace] 表"))?;

    let mut replacements = Vec::with_capacity(replace_table.len());
    for (res_path, asset_value) in replace_table {
        let asset_path = asset_value
            .as_str()
            .ok_or_else(|| anyhow!("[replace] 中的值必须是字符串: {}", res_path))?;

        // 兼容 "../assets/xxx" 或 "xxx" 两种写法
        let embedded_key = asset_path
            .trim_start_matches("../assets/")
            .trim_start_matches("./");

        let asset = Assets::get(embedded_key).ok_or_else(|| {
            anyhow!(
                "嵌入资源缺失: res_path={} asset_path={} embedded_key={}",
                res_path,
                asset_path,
                embedded_key
            )
        })?;

        replacements.push((res_path.clone(), asset.data.to_vec()));
    }

    // 读取 delete 数组（可选）
    let delete_list = table
        .get("delete")
        .map(|v| {
            if let Some(arr) = v.as_array() {
                arr.iter()
                    .map(|val| {
                        val.as_str()
                            .ok_or_else(|| anyhow!("delete 数组元素必须是字符串"))
                            .map(|s| s.to_string())
                    })
                    .collect::<Result<Vec<String>>>()
            } else if let Some(t) = v.as_table() {
                let arr = t
                    .get("paths")
                    .ok_or_else(|| anyhow!("delete 表需要 paths 数组"))?
                    .as_array()
                    .ok_or_else(|| anyhow!("delete.paths 必须是数组"))?;
                arr.iter()
                    .map(|val| {
                        val.as_str()
                            .ok_or_else(|| anyhow!("delete.paths 元素必须是字符串"))
                            .map(|s| s.to_string())
                    })
                    .collect::<Result<Vec<String>>>()
            } else {
                Err(anyhow!("delete 必须是数组或包含 paths 的表"))
            }
        })
        .transpose()?
        .unwrap_or_default();

    Ok((replacements, delete_list))
}

/// 修改指定路径 PCK 文件并应用预置替换
pub fn tweak_game_gde(file_path: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_path)
        .with_context(|| format!("无法打开文件: {}", file_path))?;

    let (header, index) = pck::read_header_and_index(&mut file).context("读取 PCK 头与索引失败")?;

    let (replacements_owned, delete_list) = load_config().context("加载 replace.toml 失败")?;

    // 先删除指定文件（若有）
    if !delete_list.is_empty() {
        pck::delete_files_in_pck(
            &mut file,
            &header,
            &index,
            delete_list.iter().map(|s| s.as_str()).collect(),
        )
        .context("删除指定文件失败")?;
    }

    // 删除可能改变 header/table，因此重新读取最新 header/index
    let (header, index) = pck::read_header_and_index(&mut file).context("删除后重读 PCK 失败")?;

    let replacements: Vec<(&str, &[u8])> = replacements_owned
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();

    pck::replace_files_in_pck(&mut file, &header, &index, replacements)
        .context("写入/替换 PCK 文件失败")?;

    Ok(())
}
