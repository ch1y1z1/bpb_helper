use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Seek, SeekFrom, Write},
};

use anyhow::{Context, Result};
use binrw::{BinRead, BinWrite};

#[derive(BinRead, Debug)]
#[br(
    magic = b"GDPC",
    little,
    assert(_reserved.iter().all(|x| *x == 0), "reserved field is not all zero"),
    assert(_version == 1, "only PCK version 1 is supported"),
    assert(file_count > 0, "no files in PCK")
)]
pub struct Header {
    _version: u32,
    _godot_version_major: u32,
    _godot_version_minor: u32,
    _godot_version_patch: u32,
    _reserved: [u32; 16],
    file_count: u32,
}

#[derive(BinRead, BinWrite, Debug, Clone)]
#[br(little)]
pub struct RawFileEntry {
    path_len: u32,

    #[br(count = path_len)]
    path_bytes: Vec<u8>,

    offset: u64,
    size: u64,

    md5: [u8; 16],
}

impl RawFileEntry {
    fn path(&self) -> Result<String> {
        Ok(String::from_utf8(self.path_bytes.clone())?
            .trim_end_matches('\0')
            .to_string())
    }
}

/// 读取 Header 和全部文件条目，返回：
/// - header
/// - entries 映射：res_path -> 在 FileTable 中该 entry 的起始偏移
pub fn read_header_and_index(file: &mut File) -> Result<(Header, HashMap<String, u64>)> {
    let mut reader = BufReader::new(file.try_clone()?);

    let header = Header::read(&mut reader).context("failed to read PCK header")?;
    println!("Header: {:?}", header);

    let mut index = HashMap::with_capacity(header.file_count as usize);

    for _ in 0..header.file_count {
        let entry_offset = reader
            .stream_position()
            .context("failed to get entry offset")?;
        let entry: RawFileEntry =
            RawFileEntry::read(&mut reader).context("failed to read RawFileEntry")?;

        let path = entry
            .path()
            .with_context(|| "invalid UTF-8 in entry path")?;

        index.insert(path, entry_offset);
    }

    Ok((header, index))
}

/// 在 PCK 中替换指定 res_path 对应的文件内容:
/// 1. 把 content 写到 PCK 尾部
/// 2. 更新该条目的 offset / size / md5
/// 3. 就地覆盖写 FileTable 中该条目
pub fn replace_file_in_pck(
    pck_file: &mut File,
    entry_offsets: &HashMap<String, u64>,
    res_path: &str,
    content: &[u8],
) -> Result<()> {
    let entry_offset = *entry_offsets
        .get(res_path)
        .ok_or_else(|| anyhow::anyhow!("entry {} not found in PCK", res_path))?;

    // 1. 追加新内容到文件尾
    let mut writer = BufWriter::new(pck_file.try_clone()?);
    writer
        .seek(SeekFrom::End(0))
        .context("failed to seek to end for data append")?;
    let new_data_offset = writer
        .stream_position()
        .context("failed to get new data offset")?;
    let new_data_size = content.len() as u64;

    writer
        .write_all(content)
        .context("failed to write new content")?;
    writer.flush().context("failed to flush new content")?;

    // 2. 从 FileTable 里读出原 entry
    let mut reader = BufReader::new(pck_file.try_clone()?);
    reader
        .seek(SeekFrom::Start(entry_offset))
        .context("failed to seek to entry offset")?;
    let mut entry: RawFileEntry =
        RawFileEntry::read(&mut reader).context("failed to re-read entry")?;

    // 3. 更新 entry 的 offset / size / md5
    entry.offset = new_data_offset;
    entry.size = new_data_size;

    // 重新计算 md5
    let digest = md5::compute(content);
    entry.md5.copy_from_slice(&digest.0);

    // 4. 回到 entry_offset 覆盖写 entry
    writer
        .seek(SeekFrom::Start(entry_offset))
        .context("failed to seek to entry offset for overwrite")?;
    entry
        .write_le(&mut writer)
        .context("failed to write updated entry")?;
    writer.flush().context("failed to flush updated entry")?;

    Ok(())
}
