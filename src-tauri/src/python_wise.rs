//! Read the historical CPython Wise payload as data. No Wise action is run.
//!
//! The ZIP central directory supplies integrity and offsets, while the binary
//! WiseScript supplies complete destination paths. Central-directory basenames
//! alone are insufficient (many standard-library files have the same name).
//! Field layouts are corroborated by the MIT-licensed SabreTools readers:
//! https://github.com/SabreTools/SabreTools.Serialization/tree/main/SabreTools.Serialization.Readers
//! and checked against the original python.org EXE bytes.

use super::{
    ExtractionOutput, MAX_ARCHIVE, MAX_FILE, MAX_FILES, checked_range, invalid, single_name,
    u16_at, u32_at,
};
use crate::error::AppResult;
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    path::PathBuf,
};

#[path = "python_wise_exe.rs"]
mod executable;
pub(super) fn is_burn(bytes: &[u8]) -> AppResult<bool> {
    executable::is_burn(bytes)
}

const MAX_SCRIPT: u64 = 8 * 1024 * 1024;
const MAX_ACTIONS: usize = 200_000;

fn crc32(bytes: &[u8]) -> u32 {
    let mut value = !0_u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(value & 1));
        }
    }
    !value
}

#[derive(Debug)]
struct LocalZip {
    end: usize,
    compressed: usize,
    size: u64,
    crc: u32,
    name: String,
    data: usize,
}
fn local_zip(bytes: &[u8], offset: usize) -> AppResult<LocalZip> {
    checked_range(offset, 30, bytes.len())?;
    if bytes.get(offset..offset + 4) != Some(b"PK\x03\x04") || u16_at(bytes, offset + 8)? != 8 {
        return Err(invalid("Wise payload 不是受支持的 DEFLATE ZIP 条目"));
    }
    let flags = u16_at(bytes, offset + 6)?;
    if flags & !0x8800 != 0 {
        return Err(invalid("Wise ZIP 加密或数据描述符不受支持"));
    }
    let compressed = u32_at(bytes, offset + 18)? as usize;
    let size = u64::from(u32_at(bytes, offset + 22)?);
    let name_len = u16_at(bytes, offset + 26)? as usize;
    let extra = u16_at(bytes, offset + 28)? as usize;
    let name_range = checked_range(offset + 30, name_len, bytes.len())?;
    let name = std::str::from_utf8(&bytes[name_range.clone()])
        .map_err(|_| invalid("Wise ZIP 名称不是 ASCII"))?
        .to_string();
    if !name.is_ascii() || (!name.is_empty() && single_name(&name, false).is_err()) {
        return Err(invalid("Wise ZIP 名称不安全"));
    }
    let data = checked_range(name_range.end, extra, bytes.len())?.end;
    let end = checked_range(data, compressed, bytes.len())?.end;
    if size > MAX_FILE || compressed as u64 > MAX_ARCHIVE {
        return Err(invalid("Wise ZIP 大小超过限制"));
    }
    Ok(LocalZip {
        end,
        compressed,
        size,
        crc: u32_at(bytes, offset + 14)?,
        name,
        data,
    })
}
fn inflate(bytes: &[u8], entry: &LocalZip, limit: u64) -> AppResult<Vec<u8>> {
    if entry.size > limit {
        return Err(invalid("Wise 解压大小超过限制"));
    }
    let mut decoder = flate2::Decompress::new(false);
    let mut value = vec![0; entry.size as usize + 1];
    let status = decoder
        .decompress(
            &bytes[entry.data..entry.end],
            &mut value,
            flate2::FlushDecompress::Finish,
        )
        .map_err(|error| invalid(format!("Wise DEFLATE 数据无效：{error}")))?;
    let used = decoder.total_in();
    // Wise's ZIP writer adds one zero byte after a few complete DEFLATE
    // streams. It is part of both declared compressed sizes and the file span.
    let padding_valid = used == entry.compressed as u64
        || (used + 1 == entry.compressed as u64 && bytes[entry.end - 1] == 0);
    if status != flate2::Status::StreamEnd || decoder.total_out() != entry.size || !padding_valid {
        return Err(invalid("Wise ZIP 压缩长度或文件大小校验失败"));
    }
    value.truncate(entry.size as usize);
    if crc32(&value) != entry.crc {
        return Err(invalid("Wise ZIP 压缩长度、文件大小或 CRC 校验失败"));
    }
    Ok(value)
}

fn central_entries(bytes: &[u8]) -> AppResult<HashMap<usize, LocalZip>> {
    // ZIP32 EOCD has a bounded 16-bit comment. Inspect only this footer window,
    // requiring the declared comment to end exactly at EOF. This is standard
    // ZIP structure parsing, not a search through the EXE for payloads.
    let mut footers = Vec::new();
    let window = bytes.len().saturating_sub(22 + u16::MAX as usize);
    for position in window..bytes.len().saturating_sub(21) {
        if bytes.get(position..position + 4) == Some(b"PK\x05\x06")
            && position + 22 + u16_at(bytes, position + 20)? as usize == bytes.len()
        {
            footers.push(position);
        }
    }
    if footers.len() != 1 {
        return Err(invalid("Wise ZIP 没有唯一且完整的中央目录结束记录"));
    }
    let footer = footers[0];
    let count = u16_at(bytes, footer + 10)? as usize;
    if u16_at(bytes, footer + 4)? != 0
        || u16_at(bytes, footer + 6)? != 0
        || u16_at(bytes, footer + 8)? as usize != count
        || count == 0
        || count == u16::MAX as usize
        || count > MAX_FILES
    {
        return Err(invalid("Wise ZIP 多磁盘、ZIP64 或文件数量不受支持"));
    }
    let directory_size = u32_at(bytes, footer + 12)? as usize;
    let directory_offset = u32_at(bytes, footer + 16)? as usize;
    let directory = checked_range(directory_offset, directory_size, bytes.len())?;
    if directory.end != footer {
        return Err(invalid("Wise ZIP 中央目录边界不匹配"));
    }
    let mut position = directory.start;
    let mut entries = HashMap::new();
    let mut ranges = Vec::new();
    for _ in 0..count {
        checked_range(position, 46, directory.end)?;
        if bytes.get(position..position + 4) != Some(b"PK\x01\x02")
            || u16_at(bytes, position + 6)? > 20
            || u16_at(bytes, position + 10)? != 8
            || u16_at(bytes, position + 34)? != 0
        {
            return Err(invalid("Wise ZIP 中央目录格式不受支持"));
        }
        let flags = u16_at(bytes, position + 8)?;
        if flags & !0x0800 != 0 {
            return Err(invalid("Wise ZIP 中央目录含加密或描述符标记"));
        }
        let filename_size = u16_at(bytes, position + 28)? as usize;
        let extra_size = u16_at(bytes, position + 30)? as usize;
        let comment_size = u16_at(bytes, position + 32)? as usize;
        let name_range = checked_range(position + 46, filename_size, directory.end)?;
        let extra = checked_range(name_range.end, extra_size, directory.end)?;
        let record_end = checked_range(extra.end, comment_size, directory.end)?.end;
        let mut extra_position = extra.start;
        while extra_position < extra.end {
            checked_range(extra_position, 4, extra.end)?;
            if u16_at(bytes, extra_position)? == 1 {
                return Err(invalid("Wise ZIP64 扩展不受支持"));
            }
            extra_position = checked_range(
                extra_position + 4,
                u16_at(bytes, extra_position + 2)? as usize,
                extra.end,
            )?
            .end;
        }
        let attributes = u32_at(bytes, position + 38)?;
        if attributes & 0x10 != 0 || (attributes >> 16) & 0o170000 == 0o120000 {
            return Err(invalid("Wise ZIP 含目录或符号链接条目"));
        }
        let offset = u32_at(bytes, position + 42)? as usize;
        let local = local_zip(bytes, offset)?;
        if local.end > directory.start
            || &bytes[name_range] != local.name.as_bytes()
            || local.size != u64::from(u32_at(bytes, position + 24)?)
            || local.compressed as u64 != u64::from(u32_at(bytes, position + 20)?)
            || local.crc != u32_at(bytes, position + 16)?
            // Wise sets its private 0x8000 bit only in the local header.
            || u16_at(bytes, offset + 6)? & !0x8000 != flags
        {
            return Err(invalid("Wise ZIP 中央目录与本地文件头不一致"));
        }
        ranges.push((offset, local.end));
        // Duplicate basenames are normal in Wise: __INIT__.PY, README, etc.
        // Preserve every physical entry; generic ZipArchive indexes by name
        // and silently replaces earlier entries that have the same basename.
        if entries.insert(offset, local).is_some() {
            return Err(invalid("Wise ZIP 条目偏移重复"));
        }
        position = record_end;
    }
    if position != directory.end {
        return Err(invalid("Wise ZIP 中央目录长度或文件数量不匹配"));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid("Wise ZIP 文件区域重叠"));
    }
    Ok(entries)
}

struct Script<'a> {
    bytes: &'a [u8],
    position: usize,
    old: bool,
}
impl<'a> Script<'a> {
    fn take(&mut self, count: usize) -> AppResult<&'a [u8]> {
        let range = checked_range(self.position, count, self.bytes.len())?;
        self.position = range.end;
        Ok(&self.bytes[range])
    }
    fn number(&mut self, count: usize) -> AppResult<u32> {
        let bytes = self.take(count)?;
        Ok(bytes
            .iter()
            .enumerate()
            .fold(0, |value, (i, byte)| value | (u32::from(*byte) << (i * 8))))
    }
    fn string(&mut self) -> AppResult<&'a str> {
        let remaining = self
            .bytes
            .get(self.position..)
            .ok_or_else(|| invalid("WiseScript 文件头被截断"))?;
        let count = remaining
            .iter()
            .take(32769)
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Wise 脚本字符串缺少结束符或过长"))?;
        let raw = self.take(count + 1)?;
        std::str::from_utf8(&raw[..count]).map_err(|_| invalid("Wise 脚本不是受支持的 ASCII 格式"))
    }
    fn strings(&mut self, count: usize) -> AppResult<()> {
        for _ in 0..count {
            self.string()?;
        }
        Ok(())
    }
    fn file(&mut self) -> AppResult<ScriptFile> {
        self.take(2)?;
        let start = self.number(4)? as usize;
        let end = self.number(4)? as usize;
        self.take(4)?;
        let size = u64::from(self.number(4)?);
        // This older Wise checksum is not the ZIP CRC. The independently
        // declared ZIP CRC is checked on every extracted payload instead.
        if !self.old {
            self.take(24)?;
        }
        let path = self.string()?.to_string();
        self.strings(2)?; // one language description and source pathname
        if end <= start || size > MAX_FILE {
            return Err(invalid("Wise 文件记录大小无效"));
        }
        Ok(ScriptFile {
            path,
            start,
            end,
            size,
        })
    }
}
#[derive(Debug)]
struct ScriptFile {
    path: String,
    start: usize,
    end: usize,
    size: u64,
}

fn script_files(bytes: &[u8], old: bool) -> AppResult<Vec<ScriptFile>> {
    if bytes.first() != Some(&0x14) {
        return Err(invalid("WiseScript 格式版本不受支持"));
    }
    let mut script = Script {
        bytes,
        position: if old { 18 } else { 43 },
        old,
    };
    if !script.string()?.is_empty()
        || script.string()? != "%MAINDIR%\\INSTALL.LOG"
        || script.string()? != "MS Sans Serif"
    {
        return Err(invalid("WiseScript 文件头不匹配官方 Python 格式"));
    }
    script.take(6)?;
    if script.number(1)? != 1 {
        return Err(invalid("WiseScript 语言数量不受支持"));
    }
    let mut python_title = false;
    for _ in 0..if old { 51 } else { 63 } {
        let value = script.string()?;
        python_title |= if old {
            ["Python 2.0", "Python 2.1"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        } else {
            ["Python 2.1", "Python 2.2", "Python 2.3"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        };
    }
    if !python_title {
        return Err(invalid("WiseScript 没有匹配的 Python 发行版标题"));
    }
    let mut files = Vec::new();
    let mut actions = 0;
    while script.position < bytes.len() {
        actions += 1;
        if actions > MAX_ACTIONS {
            return Err(invalid("WiseScript 动作数量超过限制"));
        }
        let operation = script.number(1)?;
        // Only consume field lengths. Conditions, DLL calls, programs, registry
        // and shell actions are never interpreted or executed.
        match operation {
            0 => {
                files.push(script.file()?);
                if files.len() > MAX_FILES {
                    return Err(invalid("Wise 文件数量超过限制"));
                }
            }
            2 | 0x0d | 0x0f | 0x10 | 0x1b => (),
            3 => {
                script.take(1)?;
                script.strings(2)?;
            }
            4 => {
                script.take(1)?;
                script.strings(1)?;
            }
            5 => script.strings(3)?,
            7 | 0x19 => {
                script.take(1)?;
                script.strings(3)?;
            }
            8 => {
                script.take(1)?;
            }
            9 => {
                script.take(1)?;
                script.strings(if old { 3 } else { 5 })?;
            }
            0x0a => {
                script.take(2)?;
                script.strings(3)?;
            }
            0x0b | 0x1e => {
                script.take(1)?;
                script.strings(1)?;
            }
            0x0c | 0x15 | 0x1a | 0x23 => {
                script.take(1)?;
                script.strings(2)?;
            }
            0x11 | 0x16 | 0x1c => script.strings(1)?,
            0x14 => {
                script.take(12)?;
                script.strings(2)?;
            }
            0x17 => {
                script.take(5)?;
                script.strings(1)?;
            }
            0x18 if !old => {
                if script.position < bytes.len() && [0, 255].contains(&bytes[script.position]) {
                    script.take(6)?;
                }
            }
            0x1d => script.strings(2)?,
            _ => {
                return Err(invalid(format!(
                    "WiseScript 包含未知动作 0x{operation:02x}"
                )));
            }
        }
    }
    if files.is_empty() {
        return Err(invalid("WiseScript 没有声明文件"));
    }
    Ok(files)
}

fn destination(path: &str, name: &str) -> AppResult<Option<PathBuf>> {
    if path.starts_with("%TEMP%\\") {
        return Ok(None);
    }
    if path == "%UNINSTALL_PATH%" && name.eq_ignore_ascii_case("UNWISE32.EXE") {
        return Ok(None);
    }
    let relative = if let Some(path) = path.strip_prefix("%MAINDIR%\\") {
        path
    } else if let Some(path) = path.strip_prefix("%DLLDEST%\\") {
        if path.contains('\\')
            || !(path.eq_ignore_ascii_case("MSVCRT.DLL")
                || path.eq_ignore_ascii_case("MSVCIRT.DLL")
                || (path.to_ascii_lowercase().starts_with("python2")
                    && path.to_ascii_lowercase().ends_with(".dll")))
        {
            return Err(invalid("Wise DLLDEST 不是 Python 的私有运行库"));
        }
        path
    } else {
        return Err(invalid(format!("Wise 文件目标变量不受支持：{path}")));
    };
    if relative.contains('%') {
        return Err(invalid("Wise 路径包含未解析变量"));
    }
    let mut output = PathBuf::new();
    for component in relative.split('\\') {
        output.push(single_name(component, false)?);
    }
    Ok(Some(output))
}

fn matching_name(path: &std::path::Path, archived: &str) -> bool {
    let Some(filename) = path.file_name() else {
        return false;
    };
    let filename = filename.to_string_lossy();
    if filename.eq_ignore_ascii_case(archived) {
        return true;
    }
    // Wise records explicit installation renames for these official files.
    [
        ("LICENSE", "LICENSE.txt"),
        ("README", "README.txt"),
        ("NEWS", "NEWS.txt"),
        ("PYNCHE", "pynche.py"),
        ("IDLE", "idle.pyw"),
    ]
    .iter()
    .any(|(source, target)| {
        archived.eq_ignore_ascii_case(source) && filename.eq_ignore_ascii_case(target)
    })
}

fn read_overlay(
    bytes: &[u8],
    offset: usize,
    old: bool,
) -> AppResult<(Vec<u8>, usize, HashSet<usize>)> {
    let base = if old {
        offset
    } else {
        if bytes.get(offset) != Some(&0) {
            return Err(invalid("Wise overlay DLL 名称格式不受支持"));
        }
        offset + 1
    };
    // Known official Wise 2.0 and 2.1-2.3 overlay layouts, not arbitrary ZIP
    // signatures. The PE version additionally has the DIB length and init text.
    if u32_at(bytes, base)? & 0x100 == 0 {
        return Err(invalid("Wise overlay 没有声明 PKZIP 格式"));
    }
    let script_size = u64::from(u32_at(bytes, base + 24)?);
    let script_stored = u32_at(bytes, base + 28)? as usize;
    let dll_stored = u32_at(bytes, base + 32)? as usize;
    let final_pos = if old { 52 } else { 68 };
    let final_stored = u32_at(bytes, base + final_pos)? as usize;
    let final_size = u64::from(u32_at(bytes, base + final_pos + 4)?);
    if u32_at(bytes, base + final_pos + 8)? as usize != bytes.len() {
        return Err(invalid("Wise overlay 发行包长度不匹配"));
    }
    let mut position;
    let mut declared = HashSet::new();
    if old {
        position = base + 64;
    } else {
        let dib_stored = u32_at(bytes, base + 80)? as usize;
        let dib_size = u64::from(u32_at(bytes, base + 84)?);
        if u16_at(bytes, base + 88)? != 0x0800 {
            return Err(invalid("Wise overlay 字符集标识不受支持"));
        }
        let init_len = *bytes
            .get(base + 90)
            .ok_or_else(|| invalid("Wise overlay 被截断"))? as usize;
        position = checked_range(base + 91, init_len, bytes.len())?.end;
        let dib = local_zip(bytes, position)?;
        if dib.end - position != dib_stored || dib.size != dib_size || !dib.name.is_empty() {
            return Err(invalid("Wise DIB 长度不匹配"));
        }
        inflate(bytes, &dib, MAX_SCRIPT)?;
        declared.insert(position);
        position = dib.end;
    }
    let script = local_zip(bytes, position)?;
    if script.end - position != script_stored
        || script.size != script_size
        || !script.name.is_empty()
    {
        return Err(invalid("WiseScript ZIP 长度不匹配"));
    }
    let script_bytes = inflate(bytes, &script, MAX_SCRIPT)?;
    declared.insert(position);
    position = script.end;
    let dll = local_zip(bytes, position)?;
    if dll.end - position != dll_stored || !dll.name.eq_ignore_ascii_case("WISE0001.DLL") {
        return Err(invalid("Wise 引擎 ZIP 长度不匹配"));
    }
    inflate(bytes, &dll, MAX_FILE)?;
    declared.insert(position);
    position = dll.end;
    let final_file = local_zip(bytes, position)?;
    if final_file.end - position != final_stored
        || final_file.size != final_size
        || !final_file.name.eq_ignore_ascii_case("FILE0009.DAT")
    {
        return Err(invalid("Wise 辅助数据 ZIP 长度不匹配"));
    }
    inflate(bytes, &final_file, MAX_SCRIPT)?;
    declared.insert(position);
    position = final_file.end;
    Ok((script_bytes, position, declared))
}

pub(super) fn extract(bytes: &[u8], output: &mut ExtractionOutput<'_>) -> AppResult<()> {
    let executable_offset = u32_at(bytes, 0x3c)? as usize;
    let old = bytes.get(executable_offset..executable_offset.saturating_add(2)) == Some(b"NE");
    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    for offset in executable::overlay_candidates(bytes)? {
        match read_overlay(bytes, offset, old)
            .and_then(|(script, base, declared)| Ok((script_files(&script, old)?, base, declared)))
        {
            Ok(candidate) => parsed.push(candidate),
            Err(error) => failures.push(error.to_string()),
        }
    }
    if parsed.len() != 1 {
        return Err(invalid(format!(
            "没有唯一且完整匹配的官方 Python WiseScript：{}",
            failures.join("；")
        )));
    }
    let (files, base, mut declared) = parsed.pop().unwrap();
    let entries = central_entries(bytes)?;
    let mut selected = Vec::new();
    for file in files {
        let start = base
            .checked_add(file.start)
            .ok_or_else(|| invalid("Wise 文件偏移溢出"))?;
        let end = base
            .checked_add(file.end)
            .ok_or_else(|| invalid("Wise 文件偏移溢出"))?;
        let entry = local_zip(bytes, start)?;
        if entry.end != end
            || entry.size != file.size
            || !declared.insert(start)
            || !entries.contains_key(&start)
        {
            return Err(invalid(format!(
                "WiseScript 与 ZIP 文件没有一一对应：{}，文件头偏移 {start}，声明结束 {end}，实际结束 {}，声明大小 {}，实际大小 {}",
                file.path, entry.end, file.size, entry.size
            )));
        }
        if let Some(relative) = destination(&file.path, &entry.name)? {
            if !matching_name(&relative, &entry.name) {
                return Err(invalid("WiseScript 目标文件名与 ZIP 不一致"));
            }
            output.reserve(&relative, file.size)?;
            selected.push((relative, entry));
        } else {
            inflate(bytes, &entry, MAX_FILE)?;
        }
    }
    // All remaining central entries must be known Wise dialog/config assets.
    // An unreferenced runtime payload is an error, never silently omitted.
    for (offset, entry) in entries {
        if declared.contains(&offset) {
            continue;
        }
        if !(1..=8).any(|number| {
            entry
                .name
                .eq_ignore_ascii_case(&format!("FILE{number:04}.DAT"))
        }) {
            return Err(invalid(format!("Wise 未映射文件：{}", entry.name)));
        }
        inflate(bytes, &entry, MAX_SCRIPT)?;
    }
    // Fully validate every mapping before any destination file is created.
    for (path, entry) in selected {
        let content = inflate(bytes, &entry, MAX_FILE)?;
        output.write(&path, entry.size, Cursor::new(content))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn duplicate_basename_zip() -> (Vec<u8>, Vec<usize>, usize) {
        let mut bytes = b"MZ Wise engine prefix".to_vec();
        let mut records = Vec::new();
        let name = b"__INIT__.PY";
        for content in [b"package alpha".as_slice(), b"package beta".as_slice()] {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(content).unwrap();
            let compressed = encoder.finish().unwrap();
            let offset = bytes.len();
            let crc = crc32(content);
            bytes.extend_from_slice(b"PK\x03\x04");
            for value in [20_u16, 0x8000, 8, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [crc, compressed.len() as u32, content.len() as u32] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&0_u16.to_le_bytes());
            bytes.extend_from_slice(name);
            bytes.extend_from_slice(&compressed);
            records.push((offset, crc, compressed.len(), content.len()));
        }
        let directory = bytes.len();
        for (offset, crc, compressed, size) in &records {
            bytes.extend_from_slice(b"PK\x01\x02");
            for value in [20_u16, 20, 0, 8, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [*crc, *compressed as u32, *size as u32] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [name.len() as u16, 0, 0, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&32_u32.to_le_bytes());
            bytes.extend_from_slice(&(*offset as u32).to_le_bytes());
            bytes.extend_from_slice(name);
        }
        let footer = bytes.len();
        let directory_size = footer - directory;
        bytes.extend_from_slice(b"PK\x05\x06");
        for value in [0_u16, 0, records.len() as u16, records.len() as u16] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&(directory_size as u32).to_le_bytes());
        bytes.extend_from_slice(&(directory as u32).to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        (
            bytes,
            records.iter().map(|record| record.0).collect(),
            footer,
        )
    }

    #[test]
    fn wise_central_directory_preserves_duplicate_basenames_by_physical_offset() {
        let (bytes, offsets, _) = duplicate_basename_zip();
        let entries = central_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[&offsets[0]].name, entries[&offsets[1]].name);
        assert_eq!(
            inflate(&bytes, &entries[&offsets[0]], 100).unwrap(),
            b"package alpha"
        );
        assert_eq!(
            inflate(&bytes, &entries[&offsets[1]], 100).unwrap(),
            b"package beta"
        );
    }

    #[test]
    fn wise_central_directory_rejects_duplicate_offsets_and_inconsistent_records() {
        let (bytes, _, footer) = duplicate_basename_zip();
        let directory = u32_at(&bytes, footer + 16).unwrap() as usize;
        let second = directory + 46 + b"__INIT__.PY".len();
        let mut corrupted = bytes.clone();
        corrupted[second..second + 46 + b"__INIT__.PY".len()]
            .copy_from_slice(&bytes[directory..second]);
        assert!(central_entries(&corrupted).is_err());
        for position in [
            footer + 4,
            footer + 8,
            footer + 12,
            directory + 16,
            directory + 20,
            directory + 24,
            directory + 28,
        ] {
            let mut corrupted = bytes.clone();
            corrupted[position] ^= 1;
            assert!(central_entries(&corrupted).is_err(), "offset {position}");
        }
        let mut corrupted = bytes.clone();
        corrupted[directory + 38] |= 0x10;
        assert!(central_entries(&corrupted).is_err());
        let mut corrupted = bytes;
        corrupted.extend_from_slice(b"trailing-data");
        assert!(central_entries(&corrupted).is_err());
    }

    fn old_script() -> Vec<u8> {
        let mut bytes = vec![0; 18];
        bytes[0] = 0x14;
        bytes.extend_from_slice(b"\0%MAINDIR%\\INSTALL.LOG\0MS Sans Serif\0");
        bytes.extend_from_slice(&[0; 6]);
        bytes.push(1);
        bytes.extend_from_slice(b"Python 2.0.1\0");
        bytes.extend_from_slice(&[0; 50]);
        bytes
    }

    #[test]
    fn wise_reads_actions_in_order_instead_of_scanning_path_strings() {
        let mut bytes = old_script();
        // A message containing a path must never become an InstallFile action.
        bytes.extend_from_slice(b"\x03\0%MAINDIR%\\trap.py\0message\0");
        bytes.push(0);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&40_u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&6_u32.to_le_bytes());
        bytes.extend_from_slice(b"%MAINDIR%\\Lib\\os.py\0\0\0");
        let files = script_files(&bytes, true).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "%MAINDIR%\\Lib\\os.py");
        bytes.push(0xff);
        assert!(script_files(&bytes, true).is_err());
        assert!(script_files(&[0x14], true).is_err());
    }

    #[test]
    fn wise_hidden_zip_requires_exact_compressed_size_output_size_and_crc() {
        let content = b"complete payload";
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&20_u16.to_le_bytes());
        bytes.extend_from_slice(&0x8000_u16.to_le_bytes());
        bytes.extend_from_slice(&8_u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&crc32(content).to_le_bytes());
        bytes.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(content.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&compressed);
        let entry = local_zip(&bytes, 0).unwrap();
        assert_eq!(inflate(&bytes, &entry, 100).unwrap(), content);
        bytes[14] ^= 1;
        let entry = local_zip(&bytes, 0).unwrap();
        assert!(inflate(&bytes, &entry, 100).is_err());
        bytes[14] ^= 1;
        bytes[22] += 1;
        let entry = local_zip(&bytes, 0).unwrap();
        assert!(inflate(&bytes, &entry, 100).is_err());
        bytes[22] -= 1;
        bytes.push(0);
        bytes[18] += 1;
        let entry = local_zip(&bytes, 0).unwrap();
        assert_eq!(inflate(&bytes, &entry, 100).unwrap(), content);
        *bytes.last_mut().unwrap() = 1;
        let entry = local_zip(&bytes, 0).unwrap();
        assert!(inflate(&bytes, &entry, 100).is_err());
    }

    #[test]
    fn wise_targets_remain_private_and_reject_unresolved_or_unsafe_paths() {
        assert_eq!(
            destination("%DLLDEST%\\python20.dll", "PYTHON20.DLL").unwrap(),
            Some(PathBuf::from("python20.dll"))
        );
        assert!(
            destination("%TEMP%\\helper.exe", "HELPER.EXE")
                .unwrap()
                .is_none()
        );
        for value in [
            "%SYSDIR%\\x.dll",
            "%MAINDIR%\\..\\x",
            "%MAINDIR%\\Lib\\NUL.txt",
            "%MAINDIR%\\x:stream",
            "%MAINDIR%\\%OTHER%\\x",
            "%DLLDEST%\\folder\\python20.dll",
        ] {
            assert!(destination(value, "x").is_err(), "{value}");
        }
    }
    #[test]
    fn wise_rejects_truncated_headers_and_unknown_script_formats() {
        assert!(local_zip(b"PK\x03\x04", 0).is_err());
        assert!(script_files(&[0; 100], false).is_err());
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    #[ignore = "reads official Wise EXEs from ENVNEXUS_PYTHON_ARCHIVES/wise; never starts an installer"]
    fn official_wise_python_archives_extract_only_private_runtime_files() {
        let root =
            PathBuf::from(std::env::var_os("ENVNEXUS_PYTHON_ARCHIVES").expect("archive directory"))
                .join("wise");
        for version in ["2.0.1", "2.1.3", "2.2.3", "2.3.5"] {
            let destination = tempfile::tempdir_in(&root).unwrap();
            super::super::extract(
                &root.join(format!("Python-{version}.exe")),
                destination.path(),
            )
            .unwrap();
            assert!(destination.path().join("python.exe").is_file());
            assert!(destination.path().join("Lib/os.py").is_file());
            assert!(destination.path().join("include/Python.h").is_file());
            assert!(destination.path().join("MSVCRT.DLL").is_file());
            assert!(!destination.path().join("System32").exists());
            assert!(!destination.path().join("W32INST.DLL").exists());
        }
    }
}
