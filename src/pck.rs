use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
};

use anyhow::{Context, Result, anyhow};
use binrw::{BinRead, BinWrite};
use multi_index_map::MultiIndexMap;

#[derive(BinRead, BinWrite, Debug, Clone)]
#[br(
    magic = b"GDPC",
    little,
    assert(version == 1, "only PCK version 1 is supported"),
    assert(file_count > 0, "no files in PCK")
)]
#[bw(magic = b"GDPC", little)]
pub struct Header {
    pub version: u32,
    pub godot_version_major: u32,
    pub godot_version_minor: u32,
    pub godot_version_patch: u32,
    pub reserved: [u32; 16],
    pub file_count: u32,
}

#[derive(BinRead, Debug, Clone)]
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
    /// 解析 entry 的路径字符串（去掉末尾的 NUL）
    fn path(&self) -> Result<String> {
        Ok(String::from_utf8(self.path_bytes.clone())?
            .trim_end_matches('\0')
            .to_string())
    }
}

#[derive(MultiIndexMap, Debug)]
#[multi_index_derive(Debug)]
struct EntryRecord {
    #[multi_index(hashed_unique)]
    path: String,
    #[multi_index(ordered_unique)]
    table_offset: u64,
    entry: RawFileEntry,
}

fn entry_binary_size(path_len: u32) -> u64 {
    // path_len + offset(u64) + size(u64) + md5(16) + path_len field itself
    4 + path_len as u64 + 8 + 8 + 16
}

fn normalized_path_bytes(path: &str) -> Vec<u8> {
    let mut path_bytes = path.as_bytes().to_vec();

    while path_bytes.len() % 4 != 0 {
        path_bytes.push(0);
    }

    path_bytes
}

#[derive(Debug, Clone, Copy)]
struct TablePlan {
    table_start: u64,
    table_end_after: u64,
    next_new_table_offset: u64,
}

struct AppendCtx {
    writer: File,
    reader: BufReader<File>,
    append_pos: u64,
}

impl AppendCtx {
    /// 创建读写上下文，定位到文件末尾用于追加
    fn new(pck_file: &mut File) -> Result<Self> {
        // Windows 上 try_clone 句柄共享文件指针，避免缓冲，写入前显式 seek
        let mut writer = pck_file.try_clone()?;
        writer
            .seek(SeekFrom::End(0))
            .context("failed to seek to file end")?;
        let append_pos = writer.stream_position().context("failed to get file end")?;

        let reader = BufReader::new(pck_file.try_clone()?);

        Ok(Self {
            writer,
            reader,
            append_pos,
        })
    }

    /// 将数据追加到末尾，返回起始偏移
    fn append_bytes(&mut self, data: &[u8], path: &str) -> Result<u64> {
        let offset = self.append_pos;
        self.writer
            .seek(SeekFrom::Start(offset))
            .with_context(|| format!("failed to seek writer to append_pos for {}", path))?;
        self.writer
            .write_all(data)
            .with_context(|| format!("failed to append data for {}", path))?;
        self.append_pos += data.len() as u64;
        Ok(offset)
    }

    /// 读取指定范围并复制到末尾，返回新偏移
    fn move_range(&mut self, offset: u64, size: u64, path: &str) -> Result<u64> {
        let data_size: usize = size
            .try_into()
            .map_err(|_| anyhow!("file too large to move: {}", path))?;
        let mut buf = vec![0u8; data_size];
        self.reader
            .seek(SeekFrom::Start(offset))
            .with_context(|| format!("failed to seek data for {}", path))?;
        self.reader
            .read_exact(&mut buf)
            .with_context(|| format!("failed to read data for {}", path))?;
        let dst = self.append_bytes(&buf, path)?;

        Ok(dst)
    }

    /// 刷新写缓冲
    fn flush(&mut self) -> Result<()> {
        self.writer.flush().context("failed to flush appended data")
    }
}

/// 将原有 entry 读取进多索引结构（按路径 / 表偏移）
fn build_entry_map(
    pck_file: &mut File,
    entry_offsets: &HashMap<String, u64>,
) -> Result<MultiIndexEntryRecordMap> {
    let mut reader = BufReader::new(pck_file.try_clone()?);
    let mut entry_map = MultiIndexEntryRecordMap::default();

    for (path, entry_offset) in entry_offsets {
        reader
            .seek(SeekFrom::Start(*entry_offset))
            .with_context(|| format!("failed to seek entry {}", path))?;
        let entry: RawFileEntry = RawFileEntry::read(&mut reader)
            .with_context(|| format!("failed to read entry {}", path))?;

        entry_map.insert(EntryRecord {
            path: path.clone(),
            table_offset: *entry_offset,
            entry,
        });
    }

    Ok(entry_map)
}

fn split_inputs<'a>(
    files: Vec<(&'a str, &'a [u8])>,
    entry_map: &MultiIndexEntryRecordMap,
) -> (Vec<(String, &'a [u8])>, Vec<(String, &'a [u8])>) {
    // 按是否已存在拆分为替换列表和新增列表
    let mut replace_inputs = Vec::new();
    let mut add_inputs = Vec::new();

    for (path, data) in files {
        let path_string = path.to_string();
        if entry_map.get_by_path(&path_string).is_some() {
            replace_inputs.push((path_string, data));
        } else {
            add_inputs.push((path_string, data));
        }
    }

    (replace_inputs, add_inputs)
}

fn plan_table(
    entry_map: &MultiIndexEntryRecordMap,
    add_inputs: &[(String, &[u8])],
) -> Result<TablePlan> {
    // 预估新增后表区间范围与下一个表偏移起点
    let table_start = entry_map
        .iter_by_table_offset()
        .next()
        .map(|e| e.table_offset)
        .ok_or_else(|| anyhow!("empty entry list"))?;

    let current_size: u64 = entry_map
        .iter_by_table_offset()
        .map(|e| entry_binary_size(e.entry.path_len))
        .sum();

    let additional: u64 = add_inputs
        .iter()
        .map(|(path, _)| {
            let len = normalized_path_bytes(path).len();
            entry_binary_size(len as u32)
        })
        .sum();

    let table_end_after = table_start + current_size + additional;

    Ok(TablePlan {
        table_start,
        table_end_after,
        next_new_table_offset: table_start + current_size,
    })
}

/// 读取 Header 和全部文件条目，返回：
/// - header
/// - entries 映射：res_path -> 在 FileTable 中该 entry 的起始偏移
/// 解析 PCK 头与 entry 表，返回 header 与路径到表偏移的映射
pub fn read_header_and_index(file: &mut File) -> Result<(Header, HashMap<String, u64>)> {
    let mut reader = BufReader::new(file.try_clone()?);
    reader
        .seek(SeekFrom::Start(0))
        .context("failed to seek to header start")?;

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

/// 批量替换（以及新增）PCK 中文件。
/// 流程：
/// 1. 先区分需要替换的与新增的文件
/// 2. 如果新增导致条目区间变大，则把被覆盖风险的文件数据搬到末尾
/// 3. 把新增和替换的数据统一追加到末尾，并更新/新增对应 entry
/// 4. 重写 header 的 file_count 以及完整的 entry 表
/// 批量替换/新增文件：计算迁移、追加数据并重写 entry 表与 file_count
pub fn replace_files_in_pck(
    pck_file: &mut File,
    header: &Header,
    entry_offsets: &HashMap<String, u64>,
    files: Vec<(&str, &[u8])>,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let mut dedup = HashSet::new();
    for (path, _) in &files {
        if !dedup.insert(*path) {
            return Err(anyhow!("重复的路径: {}", path));
        }
    }

    let mut entry_map = build_entry_map(pck_file, entry_offsets)?;

    let (replace_inputs, add_inputs) = split_inputs(files, &entry_map);
    let plan = plan_table(&entry_map, &add_inputs)?;
    let replace_paths: HashSet<String> = replace_inputs.iter().map(|(p, _)| p.clone()).collect();

    let mut append = AppendCtx::new(pck_file)?;

    // 2) 迁移会被新 entry 覆盖的旧文件（仅未替换的）
    let move_targets: Vec<(String, u64, u64)> = entry_map
        .iter_by_table_offset()
        .filter(|r| !replace_paths.contains(r.path.as_str()))
        .filter(|r| r.entry.offset < plan.table_end_after)
        .map(|r| (r.path.clone(), r.entry.offset, r.entry.size))
        .collect();

    for (path, offset, size) in move_targets {
        let new_offset = append.move_range(offset, size, &path)?;
        entry_map
            .update_by_path(&path, |entry: &mut RawFileEntry| {
                entry.offset = new_offset;
            })
            .ok_or_else(|| anyhow!("entry {} missing during move", path))?;
    }

    // 3) 先新增后替换，避免新增 entry 位置被重复计算
    let mut next_new_table_offset = plan.next_new_table_offset;
    for (path, data) in add_inputs.iter() {
        let mut path_bytes = normalized_path_bytes(path);
        let path_len = path_bytes.len() as u32;

        let new_offset = append.append_bytes(data, &path)?;
        let digest = md5::compute(data);
        let raw_entry = RawFileEntry {
            path_len,
            path_bytes: std::mem::take(&mut path_bytes),
            offset: new_offset,
            size: data.len() as u64,
            md5: digest.0,
        };

        entry_map.insert(EntryRecord {
            path: path.clone(),
            table_offset: next_new_table_offset,
            entry: raw_entry,
        });
        next_new_table_offset += entry_binary_size(path_len);
    }

    for (path, data) in replace_inputs {
        let new_offset = append.append_bytes(data, &path)?;
        let digest = md5::compute(data);
        entry_map
            .update_by_path(&path, |entry: &mut RawFileEntry| {
                entry.offset = new_offset;
                entry.size = data.len() as u64;
                entry.md5.copy_from_slice(&digest.0);
            })
            .ok_or_else(|| anyhow!("entry {} not found in PCK", path))?;
    }

    append.flush()?;

    // 4) 重写 header 和 entry 表
    let (_, min_data_offset) = entry_map
        .iter_by_table_offset()
        .map(|e| (e.path.clone(), e.entry.offset))
        .min_by_key(|(_, off)| *off)
        .ok_or_else(|| anyhow!("no entries to write"))?;

    let recalculated_table_size: u64 = entry_map
        .iter_by_table_offset()
        .map(|e| entry_binary_size(e.entry.path_len))
        .sum();

    if plan.table_start + recalculated_table_size > min_data_offset {
        return Err(anyhow!(
            "新 entry 表长度 {} 超出数据起始 {}，请检查迁移逻辑",
            recalculated_table_size,
            min_data_offset
        ));
    }

    // 更新 header 并整体写回
    let new_file_count: u32 = entry_map
        .len()
        .try_into()
        .map_err(|_| anyhow!("文件数量过多，超出 u32 限制"))?;

    let mut new_header = header.clone();
    new_header.file_count = new_file_count;

    {
        let mut header_writer = BufWriter::new(pck_file.try_clone()?);
        header_writer
            .seek(SeekFrom::Start(0))
            .context("failed to seek header start")?;
        new_header
            .write_le(&mut header_writer)
            .context("failed to write header")?;
        header_writer.flush().context("failed to flush header")?;
    }

    // 重写 entry 表（按 table_offset 顺序）
    let mut table_writer = BufWriter::new(pck_file.try_clone()?);
    table_writer
        .seek(SeekFrom::Start(plan.table_start))
        .context("failed to seek to entry table start")?;
    for record in entry_map.iter_by_table_offset() {
        // 手动写入每个字段以确保正确性
        record
            .entry
            .path_len
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write path_len for {}", record.path))?;
        table_writer
            .write_all(&record.entry.path_bytes)
            .with_context(|| format!("failed to write path_bytes for {}", record.path))?;
        record
            .entry
            .offset
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write offset for {}", record.path))?;
        record
            .entry
            .size
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write size for {}", record.path))?;
        table_writer
            .write_all(&record.entry.md5)
            .with_context(|| format!("failed to write md5 for {}", record.path))?;
    }
    table_writer
        .flush()
        .context("failed to flush entry table")?;

    // 5) 截断文件到正确大小（删除旧数据）
    // let final_size = entry_map
    //     .iter_by_table_offset()
    //     .map(|e| e.entry.offset + e.entry.size)
    //     .max()
    //     .unwrap_or(plan.table_start + recalculated_table_size);

    // pck_file.set_len(final_size).context("failed to truncate file")?;

    Ok(())
}

/// 删除指定路径的文件 entry，并重写 entry 表与文件数量
#[allow(dead_code)]
pub fn delete_files_in_pck(
    pck_file: &mut File,
    header: &Header,
    entry_offsets: &HashMap<String, u64>,
    paths: Vec<&str>,
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut to_remove = HashSet::new();
    for path in paths {
        if !to_remove.insert(path.to_string()) {
            return Err(anyhow!("重复的路径: {}", path));
        }
    }

    let entry_map = build_entry_map(pck_file, entry_offsets)?;

    // 仅删除实际存在的路径，不存在的静默跳过
    let existing_to_remove: HashSet<String> = to_remove
        .into_iter()
        .filter(|p| entry_map.get_by_path(p).is_some())
        .collect();

    if existing_to_remove.is_empty() {
        // 没有可删除的 entry，直接返回
        return Ok(());
    }

    let table_start = entry_map
        .iter_by_table_offset()
        .next()
        .map(|e| e.table_offset)
        .ok_or_else(|| anyhow!("empty entry list"))?;

    let mut current_offset = table_start;
    let mut remaining = Vec::new();
    for record in entry_map.iter_by_table_offset() {
        if existing_to_remove.contains(&record.path) {
            continue;
        }

        let binary_size = entry_binary_size(record.entry.path_len);
        remaining.push(EntryRecord {
            path: record.path.clone(),
            table_offset: current_offset,
            entry: record.entry.clone(),
        });
        current_offset += binary_size;
    }

    if remaining.is_empty() {
        return Err(anyhow!("删除后没有剩余文件，PCK 至少需要一个 entry"));
    }

    let recalculated_table_size = current_offset - table_start;
    let min_data_offset = remaining
        .iter()
        .map(|e| e.entry.offset)
        .min()
        .ok_or_else(|| anyhow!("no entries to write after deletion"))?;

    if table_start + recalculated_table_size > min_data_offset {
        return Err(anyhow!(
            "删除后 entry 表越界：新长度 {} 覆盖数据起点 {}",
            recalculated_table_size,
            min_data_offset
        ));
    }

    let new_file_count: u32 = remaining
        .len()
        .try_into()
        .map_err(|_| anyhow!("文件数量过多，超出 u32 限制"))?;

    let mut new_header = header.clone();
    new_header.file_count = new_file_count;

    {
        let mut header_writer = BufWriter::new(pck_file.try_clone()?);
        header_writer
            .seek(SeekFrom::Start(0))
            .context("failed to seek header start")?;
        new_header
            .write_le(&mut header_writer)
            .context("failed to write header")?;
        header_writer.flush().context("failed to flush header")?;
    }

    let mut table_writer = BufWriter::new(pck_file.try_clone()?);
    table_writer
        .seek(SeekFrom::Start(table_start))
        .context("failed to seek to entry table start")?;
    for record in &remaining {
        record
            .entry
            .path_len
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write path_len for {}", record.path))?;
        table_writer
            .write_all(&record.entry.path_bytes)
            .with_context(|| format!("failed to write path_bytes for {}", record.path))?;
        record
            .entry
            .offset
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write offset for {}", record.path))?;
        record
            .entry
            .size
            .write_le(&mut table_writer)
            .with_context(|| format!("failed to write size for {}", record.path))?;
        table_writer
            .write_all(&record.entry.md5)
            .with_context(|| format!("failed to write md5 for {}", record.path))?;
    }
    table_writer
        .flush()
        .context("failed to flush entry table")?;

    let table_end = table_start + recalculated_table_size;
    let data_end = remaining
        .iter()
        .map(|e| e.entry.offset + e.entry.size)
        .max()
        .unwrap_or(table_end);
    let final_size = table_end.max(data_end);
    pck_file
        .set_len(final_size)
        .context("failed to truncate after deletion")?;

    Ok(())
}
