//! Decode each MSI CAB folder once, using cab's checked decompressor.
//!
//! cab 0.6 restarts a folder when read_file is called for another entry. A
//! temporary one-file view exposes the original folder as one bounded stream.
//! No compressed data, folder offsets, or CFDATA checksums are modified.

use std::{
    collections::HashSet,
    io::{Cursor, Read},
    ops::Range,
};

use crate::error::{AppError, AppResult};

const MAX_CABINET_BYTES: usize = 512 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

#[derive(Clone, Copy)]
struct Limits {
    folder_bytes: u64,
    total_bytes: u64,
}

const LIMITS: Limits = Limits {
    folder_bytes: 512 * 1024 * 1024,
    total_bytes: 20 * 1024 * 1024 * 1024,
};

fn invalid(message: &str) -> AppError {
    AppError::InvalidSource(format!("Python MSI CAB：{message}"))
}

fn range(start: usize, length: usize, end: usize) -> AppResult<Range<usize>> {
    let next = start
        .checked_add(length)
        .filter(|next| *next <= end)
        .ok_or_else(|| invalid("文件头或压缩数据被截断"))?;
    Ok(start..next)
}

fn u16_at(bytes: &[u8], at: usize) -> AppResult<u16> {
    let raw = &bytes[range(at, 2, bytes.len())?];
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> AppResult<u32> {
    let raw = &bytes[range(at, 4, bytes.len())?];
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

#[derive(Debug)]
struct Entry {
    name: String,
    offset: u32,
    size: u32,
}

#[derive(Debug)]
struct Folder {
    entries: Vec<usize>,
    file_end: u32,
}

#[derive(Debug)]
struct Layout {
    cabinet_size: usize,
    file_table: usize,
    entries: Vec<Entry>,
    folders: Vec<Folder>,
}

fn layout(bytes: &[u8], limits: Limits) -> AppResult<Layout> {
    if bytes.len() > MAX_CABINET_BYTES || bytes.get(..4) != Some(b"MSCF") {
        return Err(invalid("CAB 签名无效或文件超过大小限制"));
    }
    range(0, 36, bytes.len())?;
    if u32_at(bytes, 4)? != 0 || u32_at(bytes, 12)? != 0 || u32_at(bytes, 20)? != 0 {
        return Err(invalid("CAB 保留字段无效"));
    }
    let cabinet_size = u32_at(bytes, 8)? as usize;
    range(0, cabinet_size, bytes.len())?;
    if cabinet_size < 36 {
        return Err(invalid("CAB 声明大小无效"));
    }
    let bytes = &bytes[..cabinet_size];
    let file_table = u32_at(bytes, 16)? as usize;
    let folder_count = u16_at(bytes, 26)? as usize;
    let file_count = u16_at(bytes, 28)? as usize;
    let flags = u16_at(bytes, 30)?;
    if flags & 3 != 0 || flags & !7 != 0 {
        return Err(invalid("不接受分卷 CAB 或未知保留标志"));
    }
    if folder_count == 0 || file_count == 0 || file_count > MAX_FILES {
        return Err(invalid("CAB 文件夹或文件数量无效"));
    }
    let (folder_table, folder_reserve, data_reserve) = if flags & 4 != 0 {
        range(36, 4, cabinet_size)?;
        (
            range(40, u16_at(bytes, 36)? as usize, cabinet_size)?.end,
            bytes[38] as usize,
            bytes[39] as usize,
        )
    } else {
        (36, 0, 0)
    };
    let folder_width = 8 + folder_reserve;
    let folder_table_end = range(folder_table, folder_count * folder_width, cabinet_size)?.end;
    if file_table < folder_table_end {
        return Err(invalid("CAB 文件表与文件头重叠"));
    }
    let mut folders = (0..folder_count)
        .map(|_| Folder {
            entries: Vec::new(),
            file_end: 0,
        })
        .collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(file_count);
    let mut at = file_table;
    for _ in 0..file_count {
        range(at, 16, cabinet_size)?;
        let size = u32_at(bytes, at)?;
        let offset = u32_at(bytes, at + 4)?;
        let folder = u16_at(bytes, at + 8)? as usize;
        if folder >= folder_count {
            return Err(invalid("文件引用未知文件夹或分卷文件夹"));
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| invalid("CAB 文件偏移溢出"))?;
        if end as u64 > limits.folder_bytes {
            return Err(invalid("CAB 文件夹超过解压大小限制"));
        }
        let name_at = at + 16;
        let name_length = bytes[name_at..]
            .iter()
            .position(|byte| *byte == 0)
            .filter(|length| *length > 0 && *length <= 4096)
            .ok_or_else(|| invalid("CAB 文件名为空、过长或被截断"))?;
        at = range(name_at, name_length + 1, cabinet_size)?.end;
        folders[folder].entries.push(entries.len());
        folders[folder].file_end = folders[folder].file_end.max(end);
        entries.push(Entry {
            name: String::new(),
            offset,
            size,
        });
    }
    let file_table_end = at;
    let mut compressed_ranges = Vec::new();
    let mut total = 0u64;
    for (index, folder) in folders.iter().enumerate() {
        let header = folder_table + index * folder_width;
        let mut block = u32_at(bytes, header)? as usize;
        let blocks = u16_at(bytes, header + 4)? as usize;
        if block < file_table_end || block > cabinet_size {
            return Err(invalid("CAB 文件夹数据与文件表重叠或越界"));
        }
        let start = block;
        let mut uncompressed = 0u64;
        for _ in 0..blocks {
            range(block, 8 + data_reserve, cabinet_size)?;
            let compressed = u16_at(bytes, block + 4)? as usize;
            let size = u16_at(bytes, block + 6)? as u64;
            if compressed == 0 || size == 0 || size > 32768 {
                return Err(invalid("CAB 数据块大小无效或属于分卷数据"));
            }
            block = range(block + 8 + data_reserve, compressed, cabinet_size)?.end;
            uncompressed = uncompressed
                .checked_add(size)
                .ok_or_else(|| invalid("CAB 文件夹大小溢出"))?;
            if uncompressed > limits.folder_bytes {
                return Err(invalid("CAB 文件夹超过解压大小限制"));
            }
        }
        if folder.file_end as u64 > uncompressed {
            return Err(invalid("CAB 文件超出文件夹解压范围"));
        }
        total = total
            .checked_add(uncompressed)
            .ok_or_else(|| invalid("CAB 总大小溢出"))?;
        if total > limits.total_bytes {
            return Err(invalid("CAB 总解压大小超过限制"));
        }
        if block > start {
            compressed_ranges.push(start..block);
        }
        let mut sorted = folder.entries.clone();
        sorted.sort_by_key(|index| entries[*index].offset);
        let mut previous_start = 0;
        let mut previous_end = 0;
        for entry_index in sorted {
            let entry = &entries[entry_index];
            if entry.size == 0 {
                continue;
            }
            let end = entry.offset + entry.size;
            // Official WiX CABs deduplicate identical contents under multiple
            // file IDs. Exact aliases share the same bytes; partial overlaps
            // or different lengths at the same offset are not valid aliases.
            if entry.offset < previous_end
                && !(entry.offset == previous_start && end == previous_end)
            {
                return Err(invalid("CAB 文件在文件夹内重叠"));
            }
            previous_start = entry.offset;
            previous_end = end;
        }
    }
    compressed_ranges.sort_by_key(|range| range.start);
    if compressed_ranges
        .windows(2)
        .any(|pair| pair[1].start < pair[0].end)
    {
        return Err(invalid("CAB 文件夹压缩数据重叠"));
    }
    // Let cab retain its original filename decoding and format validation.
    // Its per-folder entries preserve the original CFFILE table order.
    let cabinet = cab::Cabinet::new(Cursor::new(bytes))
        .map_err(|error| invalid(&format!("CAB 无效：{error}")))?;
    let mut names = HashSet::new();
    for (index, folder) in cabinet.folder_entries().enumerate() {
        let file_entries = folder.file_entries().collect::<Vec<_>>();
        if file_entries.len() != folders[index].entries.len() {
            return Err(invalid("CAB 文件表与解码器映射不一致"));
        }
        for (entry_index, file) in folders[index].entries.iter().zip(file_entries) {
            let entry = &mut entries[*entry_index];
            if file.uncompressed_size() != entry.size || !names.insert(file.name().to_string()) {
                return Err(invalid("CAB 文件 ID 重复或大小不一致"));
            }
            entry.name = file.name().to_string();
        }
    }
    Ok(Layout {
        cabinet_size,
        file_table,
        entries,
        folders,
    })
}

fn decode_folder(view: &[u8], size: u32) -> AppResult<Vec<u8>> {
    let mut cabinet = cab::Cabinet::new(Cursor::new(view))
        .map_err(|error| invalid(&format!("CAB 文件夹视图无效：{error}")))?;
    let mut reader = cabinet
        .read_file("x")
        .map_err(|error| invalid(&format!("CAB 文件夹读取失败：{error}")))?;
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(size as usize)
        .map_err(|_| invalid("无法分配受限 CAB 文件夹缓冲区"))?;
    decoded.resize(size as usize, 0);
    reader
        .read_exact(&mut decoded)
        .map_err(|error| invalid(&format!("CAB 文件夹解码或校验失败：{error}")))?;
    Ok(decoded)
}

pub(super) fn for_each_file<F>(bytes: &[u8], wanted: &HashSet<String>, visit: F) -> AppResult<()>
where
    F: FnMut(&str, &[u8]) -> AppResult<()>,
{
    for_each_file_using(bytes, wanted, LIMITS, decode_folder, visit)
}

fn for_each_file_using<F, D>(
    bytes: &[u8],
    wanted: &HashSet<String>,
    limits: Limits,
    mut decode: D,
    mut visit: F,
) -> AppResult<()>
where
    F: FnMut(&str, &[u8]) -> AppResult<()>,
    D: FnMut(&[u8], u32) -> AppResult<Vec<u8>>,
{
    let layout = layout(bytes, limits)?;
    let known = layout
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<HashSet<_>>();
    if wanted.iter().any(|name| !known.contains(name.as_str())) {
        return Err(invalid("CAB 缺少 MSI 声明的文件 ID"));
    }
    let mut view = bytes[..layout.cabinet_size].to_vec();
    view[28..30].copy_from_slice(&1u16.to_le_bytes());
    for (index, folder) in layout.folders.iter().enumerate() {
        if !folder
            .entries
            .iter()
            .any(|entry| wanted.contains(&layout.entries[*entry].name))
        {
            continue;
        }
        let at = layout.file_table;
        view[at..at + 4].copy_from_slice(&folder.file_end.to_le_bytes());
        view[at + 4..at + 8].copy_from_slice(&0u32.to_le_bytes());
        view[at + 8..at + 10].copy_from_slice(&(index as u16).to_le_bytes());
        view[at + 16..at + 18].copy_from_slice(b"x\0");
        let decoded = decode(&view, folder.file_end)?;
        if decoded.len() != folder.file_end as usize {
            return Err(invalid("CAB 文件夹实际解压大小不符"));
        }
        for entry_index in &folder.entries {
            let entry = &layout.entries[*entry_index];
            if wanted.contains(&entry.name) {
                let content =
                    &decoded[range(entry.offset as usize, entry.size as usize, decoded.len())?];
                visit(&entry.name, content)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, io::Write};

    fn fixture(groups: &[&[(&str, &[u8])]]) -> Vec<u8> {
        let mut builder = cab::CabinetBuilder::new();
        let mut inputs = HashMap::new();
        for group in groups {
            let folder = builder.add_folder(cab::CompressionType::MsZip);
            for (name, content) in *group {
                folder.add_file(*name);
                inputs.insert(*name, *content);
            }
        }
        let mut writer = builder.build(Cursor::new(Vec::new())).unwrap();
        while let Some(mut file) = writer.next_file().unwrap() {
            file.write_all(inputs[file.file_name()]).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn wanted(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn decodes_one_stream_per_selected_folder_and_preserves_every_file() {
        let bytes = fixture(&[
            &[("first", b"one"), ("second", b"two"), ("third", b"three")],
            &[("fourth", b"four"), ("fifth", b"five")],
        ]);
        let selected = wanted(&["first", "second", "third", "fifth"]);
        let mut calls = 0;
        let mut files = HashMap::new();
        for_each_file_using(
            &bytes,
            &selected,
            LIMITS,
            |view, size| {
                calls += 1;
                decode_folder(view, size)
            },
            |name, content| {
                files.insert(name.to_string(), content.to_vec());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(files.len(), 4);
        assert_eq!(files["first"], b"one");
        assert_eq!(files["third"], b"three");
        assert_eq!(files["fifth"], b"five");
        assert_eq!(layout(&bytes, LIMITS).unwrap().folders.len(), 2);
        let mut skipped = 0;
        for_each_file_using(
            &bytes,
            &wanted(&["first", "third"]),
            LIMITS,
            |view, size| {
                skipped += 1;
                decode_folder(view, size)
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(skipped, 1);
    }

    #[test]
    fn checks_the_last_file_data_even_when_only_an_earlier_file_is_selected() {
        let mut bytes = fixture(&[&[("first", b"one"), ("last", b"last data")]]);
        let last = bytes.len() - 1;
        bytes[last] ^= 0x5a;
        let mut visited = false;
        assert!(
            for_each_file(&bytes, &wanted(&["first"]), |_, _| {
                visited = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!visited);
    }

    #[test]
    fn preserves_exact_content_aliases_used_by_official_wix_cabinets() {
        let mut bytes = fixture(&[&[("first", b"same"), ("alias", b"same")]]);
        let table = u32_at(&bytes, 16).unwrap() as usize;
        let second = table + 16 + "first".len() + 1;
        bytes[second + 4..second + 8].copy_from_slice(&0u32.to_le_bytes());
        let mut files = HashMap::new();
        for_each_file(&bytes, &wanted(&["first", "alias"]), |name, content| {
            files.insert(name.to_string(), content.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(files["first"], b"same");
        assert_eq!(files["alias"], b"same");
    }

    #[test]
    fn rejects_truncation_missing_names_and_bounded_folder_or_total_limits() {
        let bytes = fixture(&[&[("first", b"12345678")], &[("last", b"abcdefgh")]]);
        assert!(layout(&bytes[..bytes.len() - 1], LIMITS).is_err());
        assert!(
            layout(
                &bytes,
                Limits {
                    folder_bytes: 7,
                    total_bytes: 100,
                }
            )
            .is_err()
        );
        assert!(
            layout(
                &bytes,
                Limits {
                    folder_bytes: 8,
                    total_bytes: 15,
                }
            )
            .is_err()
        );
        assert!(for_each_file(&bytes, &wanted(&["missing"]), |_, _| Ok(())).is_err());
    }

    #[test]
    fn rejects_file_overlap_split_folders_flags_and_integer_overflow() {
        let original = fixture(&[&[("first", b"first"), ("second", b"second")]]);
        let table = u32_at(&original, 16).unwrap() as usize;
        let second = table + 16 + "first".len() + 1;
        let mut overlap = original.clone();
        overlap[second + 4..second + 8].copy_from_slice(&1u32.to_le_bytes());
        assert!(layout(&overlap, LIMITS).is_err());
        let mut split = original.clone();
        split[table + 8..table + 10].copy_from_slice(&0xfffeu16.to_le_bytes());
        assert!(layout(&split, LIMITS).is_err());
        for flag in [1u16, 2, 8] {
            let mut flags = original.clone();
            flags[30..32].copy_from_slice(&flag.to_le_bytes());
            assert!(layout(&flags, LIMITS).is_err());
        }
        let mut truncated_name = original.clone();
        truncated_name[table + 16..].fill(b'x');
        assert!(layout(&truncated_name, LIMITS).is_err());
        let mut overflow = original;
        overflow[table + 4..table + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(layout(&overflow, LIMITS).is_err());
    }
}
