use crate::pck;
use anyhow::{Context, Result};
use std::fs::OpenOptions;

macro_rules! replace_files {
    ( $pck_file:expr, $entry_offsets:expr, { $( $res_path:literal => $asset_path:literal ),* $(,)? } ) => {{
        $(
            {
                let replace_content: &[u8] = include_bytes!($asset_path);
                $crate::pck::replace_file_in_pck($pck_file, $entry_offsets, $res_path, replace_content)?;
            }
        )*
    }};
}

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

    replace_files!(&mut file, &index, {
        "res://Core/Game.gde" => "../assets/Game.gde",
        "res://Interface/ItemLibrary/ItemLibrary.gde" => "../assets/ItemLibrary.gde",
        "res://Core/Main.tscn" => "../assets/Main.tscn",
        "res://Interface/BuildHistory/BuildHistory.gde" => "../assets/BuildHistory.gde",
        "res://Interface/BuildHistory/BuildHistory.tscn" => "../assets/BuildHistory.tscn",
        "res://Interface/BuildHistory/BuildHistoryData.gde" => "../assets/BuildHistoryData.gde",
    });

    Ok(())
}
