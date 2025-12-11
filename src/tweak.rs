use crate::pck;
use anyhow::{Context, Result};
use std::fs::OpenOptions;

/// 修改指定路径 pck 文件
pub fn tweak_game_gde(file_path: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_path)
        .with_context(|| format!("无法打开文件: {}", file_path))?;

    let (_header, index) = pck::read_header_and_index(&mut file)?;

    // index.iter().for_each(|(path, offset)| {
    //     println!("Path: {}, Offset: {}", path, offset);
    // });

    let replace_content = include_bytes!("../assets/Game.gde");
    pck::replace_file_in_pck(&mut file, &index, "res://Core/Game.gde", replace_content)?;

    let replace_content = include_bytes!("../assets/ItemLibrary.gde");
    pck::replace_file_in_pck(
        &mut file,
        &index,
        "res://Interface/ItemLibrary/ItemLibrary.gde",
        replace_content,
    )?;

    Ok(())
}
