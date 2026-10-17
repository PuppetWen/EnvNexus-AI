//! Bounded executable structure parsing for historical Wise payload locations.
//!
//! Offsets come from PE sections or NE segments/resources, never from searching
//! executable bytes for an archive signature. In particular, both the NE
//! resource offset and its length use the resource table's alignment units.

use std::ops::Range;

use crate::error::{AppError, AppResult};

fn invalid(message: &str) -> AppError {
    AppError::InvalidSource(format!("Python Wise 文件头：{message}"))
}

fn range(bytes: &[u8], start: usize, length: usize) -> AppResult<Range<usize>> {
    let end = start
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("表项超出文件边界"))?;
    Ok(start..end)
}

fn u16_at(bytes: &[u8], offset: usize) -> AppResult<u16> {
    let value = &bytes[range(bytes, offset, 2)?];
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> AppResult<u32> {
    let value = &bytes[range(bytes, offset, 4)?];
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn executable_header(bytes: &[u8]) -> AppResult<usize> {
    if bytes.get(..2) != Some(b"MZ") {
        return Err(invalid("缺少 DOS 文件头"));
    }
    let offset = u32_at(bytes, 0x3c)? as usize;
    if offset < 0x40 {
        return Err(invalid("扩展文件头与 DOS 文件头重叠"));
    }
    range(bytes, offset, 2)?;
    Ok(offset)
}

struct PeLayout {
    overlay: usize,
    burn: bool,
}

fn pe_layout(bytes: &[u8], pe: usize) -> AppResult<PeLayout> {
    if bytes.get(range(bytes, pe, 4)?) != Some(b"PE\0\0") {
        return Err(invalid("PE 签名无效"));
    }
    range(bytes, pe, 24)?;
    let count = u16_at(bytes, pe + 6)? as usize;
    if !(1..=96).contains(&count) {
        return Err(invalid("PE 分节数量无效"));
    }
    let optional_size = u16_at(bytes, pe + 20)? as usize;
    let optional = pe + 24;
    range(bytes, optional, optional_size)?;
    let minimum = match u16_at(bytes, optional)? {
        0x10b => 96,
        0x20b => 112,
        _ => return Err(invalid("PE 可选头格式无效")),
    };
    if optional_size < minimum {
        return Err(invalid("PE 可选头被截断"));
    }
    let table = optional + optional_size;
    let table_end = range(bytes, table, count * 40)?.end;
    let declared_headers = u32_at(bytes, optional + 60)? as usize;
    if declared_headers != 0 && declared_headers < table_end {
        return Err(invalid("PE 声明的文件头长度不足"));
    }
    // Some minimal historical stubs leave SizeOfHeaders unset. The section
    // table itself still supplies a strict lower bound for all raw sections.
    let headers = declared_headers.max(table_end);
    range(bytes, 0, headers)?;
    let mut overlay = headers;
    let mut burn = false;
    let mut sections = Vec::<Range<usize>>::with_capacity(count);
    for index in 0..count {
        let entry = table + index * 40;
        let size = u32_at(bytes, entry + 16)? as usize;
        let offset = u32_at(bytes, entry + 20)? as usize;
        let is_burn_section = &bytes[entry..entry + 8] == b".wixburn";
        if is_burn_section {
            if burn || size == 0 {
                return Err(invalid("Burn 分节重复或为空"));
            }
            burn = true;
        }
        if size == 0 {
            continue;
        }
        let section = range(bytes, offset, size)?;
        if offset < headers
            || sections
                .iter()
                .any(|other| section.start < other.end && other.start < section.end)
        {
            return Err(invalid("PE 分节与文件头或其他分节重叠"));
        }
        overlay = overlay.max(section.end);
        sections.push(section);
    }
    if sections.is_empty() {
        return Err(invalid("PE 缺少文件中的分节数据"));
    }
    Ok(PeLayout { overlay, burn })
}

fn shifted(value: usize, shift: u16) -> AppResult<usize> {
    if shift > 16 {
        return Err(invalid("NE 对齐单位过大"));
    }
    value
        .checked_mul(1_usize << shift)
        .ok_or_else(|| invalid("NE 对齐偏移溢出"))
}

fn relative(bytes: &[u8], ne: usize, offset: u16, size: usize) -> AppResult<Range<usize>> {
    if offset < 64 {
        return Err(invalid("NE 表与文件头重叠"));
    }
    let start = ne
        .checked_add(offset as usize)
        .ok_or_else(|| invalid("NE 表偏移溢出"))?;
    range(bytes, start, size)
}

fn name_table(bytes: &[u8], table: Range<usize>) -> AppResult<usize> {
    let mut cursor = table.start;
    while cursor < table.end {
        let length = bytes[cursor] as usize;
        cursor += 1;
        if length == 0 {
            return Ok(cursor);
        }
        cursor = cursor
            .checked_add(length + 2)
            .filter(|end| *end <= table.end)
            .ok_or_else(|| invalid("NE 名称表被截断"))?;
    }
    Err(invalid("NE 名称表缺少结束标记"))
}

fn ne_candidates(bytes: &[u8], ne: usize) -> AppResult<Vec<usize>> {
    range(bytes, ne, 64)?;
    let count = u16_at(bytes, ne + 0x1c)? as usize;
    if !(1..=4096).contains(&count) {
        return Err(invalid("NE 段数量无效"));
    }
    let shift = match u16_at(bytes, ne + 0x32)? {
        0 => 9,
        value => value,
    };
    shifted(1, shift)?;
    let segments = relative(bytes, ne, u16_at(bytes, ne + 0x22)?, count * 8)?;
    let entry_size = u16_at(bytes, ne + 0x06)? as usize;
    let entry_table = relative(bytes, ne, u16_at(bytes, ne + 0x04)?, entry_size)?;
    let resource_offset = u16_at(bytes, ne + 0x24)?;
    let resident_offset = u16_at(bytes, ne + 0x26)?;
    let module_offset = u16_at(bytes, ne + 0x28)?;
    let imported_offset = u16_at(bytes, ne + 0x2a)?;
    if resource_offset >= resident_offset
        || resident_offset >= module_offset
        || module_offset > imported_offset
    {
        return Err(invalid("NE 表顺序无效"));
    }
    let resources = relative(
        bytes,
        ne,
        resource_offset,
        (resident_offset - resource_offset) as usize,
    )?;
    let residents = relative(
        bytes,
        ne,
        resident_offset,
        (module_offset - resident_offset) as usize,
    )?;
    let modules = relative(
        bytes,
        ne,
        module_offset,
        (u16_at(bytes, ne + 0x1e)? as usize) * 2,
    )?;
    let imported = relative(bytes, ne, imported_offset, 0)?.start;
    if modules.end > imported {
        return Err(invalid("NE 模块引用表重叠"));
    }
    let resident_end = name_table(bytes, residents.clone())?;
    let mut metadata_end = entry_table.end.max(segments.end).max(resources.end);
    metadata_end = metadata_end.max(resident_end).max(modules.end);
    let mut metadata = vec![
        0..ne + 64,
        entry_table,
        segments.clone(),
        resources.clone(),
        residents,
        modules.clone(),
    ];
    for module in (modules.start..modules.end).step_by(2) {
        let name = imported
            .checked_add(u16_at(bytes, module)? as usize)
            .ok_or_else(|| invalid("NE 导入名称偏移溢出"))?;
        let length = *bytes
            .get(name)
            .ok_or_else(|| invalid("NE 导入名称被截断"))? as usize;
        let name = range(bytes, name, length + 1)?;
        metadata_end = metadata_end.max(name.end);
        metadata.push(name);
    }
    let nonresident_start = u32_at(bytes, ne + 0x2c)? as usize;
    let nonresident_size = u16_at(bytes, ne + 0x20)? as usize;
    if nonresident_size > 0 {
        if nonresident_start < ne + 64 {
            return Err(invalid("NE 非驻留名称表与文件头重叠"));
        }
        let nonresident = range(bytes, nonresident_start, nonresident_size)?;
        name_table(bytes, nonresident.clone())?;
        metadata_end = metadata_end.max(nonresident.end);
        metadata.push(nonresident);
    }
    let mut data = Vec::<Range<usize>>::with_capacity(count);
    let mut end = metadata_end;
    for index in 0..count {
        let entry = segments.start + index * 8;
        let sector = u16_at(bytes, entry)? as usize;
        let flags = u16_at(bytes, entry + 4)?;
        if sector == 0 {
            if flags & 0x0100 != 0 {
                return Err(invalid("未初始化的 NE 段包含重定位数据"));
            }
            continue;
        }
        let size = match u16_at(bytes, entry + 2)? {
            0 => 65_536,
            value => value as usize,
        };
        let start = shifted(sector, shift)?;
        let mut segment = range(bytes, start, size)?;
        if flags & 0x0100 != 0 {
            let records = u16_at(bytes, segment.end)? as usize;
            segment.end = range(bytes, segment.end, 2 + records * 8)?.end;
        }
        end = end.max(segment.end);
        data.push(segment);
    }
    if resources.len() < 4 {
        return Err(invalid("NE 资源表被截断"));
    }
    let resource_shift = u16_at(bytes, resources.start)?;
    shifted(1, resource_shift)?;
    let mut cursor = resources.start + 2;
    let mut resource_count = 0_usize;
    loop {
        if cursor + 2 > resources.end {
            return Err(invalid("NE 资源表缺少结束标记"));
        }
        let kind = u16_at(bytes, cursor)?;
        cursor += 2;
        if kind == 0 {
            break;
        }
        if cursor + 6 > resources.end {
            return Err(invalid("NE 资源类型被截断"));
        }
        let count = u16_at(bytes, cursor)? as usize;
        resource_count += count;
        if resource_count > 100_000 {
            return Err(invalid("NE 资源数量过大"));
        }
        cursor += 6;
        let entries_end = range(bytes, cursor, count * 12)?.end;
        if entries_end > resources.end {
            return Err(invalid("NE 资源条目越过资源表"));
        }
        for entry in (cursor..entries_end).step_by(12) {
            let offset = shifted(u16_at(bytes, entry)? as usize, resource_shift)?;
            let size = shifted(u16_at(bytes, entry + 2)? as usize, resource_shift)?;
            if size == 0 {
                return Err(invalid("NE 资源长度为零"));
            }
            let resource = range(bytes, offset, size)?;
            end = end.max(resource.end);
            data.push(resource);
        }
        cursor = entries_end;
    }
    if data.is_empty()
        || data.iter().any(|item| {
            metadata
                .iter()
                .any(|table| !table.is_empty() && item.start < table.end && table.start < item.end)
        })
    {
        return Err(invalid("NE 数据缺失或与表重叠"));
    }
    let mut candidates = vec![end];
    // A linker may insert alignment padding after the last relocation record.
    // Only declared alignment boundaries with entirely zero padding qualify;
    // there is no arbitrary byte/signature search through the overlay.
    for alignment in [shift, resource_shift] {
        let unit = shifted(1, alignment)?;
        let rounded = end
            .checked_add(unit - 1)
            .map(|value| value & !(unit - 1))
            .ok_or_else(|| invalid("NE 覆盖层对齐溢出"))?;
        if rounded
            .checked_add(64)
            .is_some_and(|end| end <= bytes.len())
            && bytes[range(bytes, end, rounded - end)?]
                .iter()
                .all(|byte| *byte == 0)
        {
            candidates.push(rounded);
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    Ok(candidates)
}

pub(super) fn overlay_candidates(bytes: &[u8]) -> AppResult<Vec<usize>> {
    let header = executable_header(bytes)?;
    let candidates = match bytes.get(range(bytes, header, 2)?) {
        Some(b"PE") => vec![pe_layout(bytes, header)?.overlay],
        Some(b"NE") => ne_candidates(bytes, header)?,
        _ => return Err(invalid("不支持的扩展可执行文件头")),
    };
    if candidates.iter().any(|offset| {
        offset
            .checked_add(64)
            .is_none_or(|minimum| minimum > bytes.len())
    }) {
        return Err(invalid("覆盖层缺少完整 Wise 文件头"));
    }
    Ok(candidates)
}

pub(super) fn is_burn(bytes: &[u8]) -> AppResult<bool> {
    let header = executable_header(bytes)?;
    match bytes.get(range(bytes, header, 2)?) {
        Some(b"PE") => Ok(pe_layout(bytes, header)?.burn),
        Some(b"NE") => Ok(false),
        _ => Err(invalid("不支持的扩展可执行文件头")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn set32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn pe() -> Vec<u8> {
        let mut bytes = vec![0; 0x480];
        bytes[..2].copy_from_slice(b"MZ");
        set32(&mut bytes, 0x3c, 0x80);
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        set16(&mut bytes, 0x86, 2);
        set16(&mut bytes, 0x94, 0xe0);
        set16(&mut bytes, 0x98, 0x10b);
        set32(&mut bytes, 0xd4, 0x200);
        bytes[0x178..0x17d].copy_from_slice(b".text");
        set32(&mut bytes, 0x188, 0x100);
        set32(&mut bytes, 0x18c, 0x200);
        bytes[0x1a0..0x1a5].copy_from_slice(b".data");
        set32(&mut bytes, 0x1b0, 0x100);
        set32(&mut bytes, 0x1b4, 0x300);
        bytes[0x220..0x224].copy_from_slice(b"PK\x03\x04");
        bytes
    }

    fn ne() -> Vec<u8> {
        let mut bytes = vec![0; 0x380];
        bytes[..2].copy_from_slice(b"MZ");
        set32(&mut bytes, 0x3c, 0x80);
        bytes[0x80..0x82].copy_from_slice(b"NE");
        set16(&mut bytes, 0x84, 0x70); // entry table, 0xf0
        set16(&mut bytes, 0x86, 2);
        set16(&mut bytes, 0x9c, 1); // segments
        set16(&mut bytes, 0xa0, 1); // nonresident name table size
        set16(&mut bytes, 0xa2, 0x40); // segment table, 0xc0
        set16(&mut bytes, 0xa4, 0x48); // resource table, 0xc8
        set16(&mut bytes, 0xa6, 0x60); // resident names, 0xe0
        set16(&mut bytes, 0xa8, 0x62); // module refs, 0xe2
        set16(&mut bytes, 0xaa, 0x62); // imported names, 0xe2
        set32(&mut bytes, 0xac, 0xf2); // nonresident names
        set16(&mut bytes, 0xb2, 4); // segment alignment
        set16(&mut bytes, 0xc0, 0x10); // segment data 0x100
        set16(&mut bytes, 0xc2, 0x20);
        set16(&mut bytes, 0xc4, 0x100); // relocation count at 0x120
        set16(&mut bytes, 0x120, 1); // relocation end 0x12a
        set16(&mut bytes, 0xc8, 4); // resource alignment
        set16(&mut bytes, 0xca, 0x8003);
        set16(&mut bytes, 0xcc, 1);
        set16(&mut bytes, 0xd2, 0x20); // resource 0x200
        set16(&mut bytes, 0xd4, 0x10); // length 0x100, end 0x300
        set16(&mut bytes, 0xd8, 0x8001);
        bytes
    }

    #[test]
    fn pe_uses_raw_section_end_and_only_structured_burn_section() {
        let mut bytes = pe();
        assert_eq!(overlay_candidates(&bytes).unwrap(), [0x400]);
        assert!(!is_burn(&bytes).unwrap());
        bytes[0x210..0x218].copy_from_slice(b".wixburn");
        assert!(!is_burn(&bytes).unwrap());
        bytes[0x1a0..0x1a8].copy_from_slice(b".wixburn");
        assert!(is_burn(&bytes).unwrap());
        bytes[0x178..0x180].copy_from_slice(b".wixburn");
        assert!(is_burn(&bytes).is_err());
    }

    #[test]
    fn ne_scales_resource_length_and_includes_relocations_and_name_table() {
        let mut bytes = ne();
        assert_eq!(overlay_candidates(&bytes).unwrap(), [0x300]);
        assert!(!is_burn(&bytes).unwrap());
        // With a short resource, the relocation end is the true last data byte.
        set16(&mut bytes, 0xd2, 0x10);
        set16(&mut bytes, 0xd4, 1);
        assert_eq!(overlay_candidates(&bytes).unwrap(), [0x12a, 0x130]);
        bytes[0x12a] = 1;
        assert_eq!(overlay_candidates(&bytes).unwrap(), [0x12a]);
        set32(&mut bytes, 0xac, 0x310);
        assert_eq!(overlay_candidates(&bytes).unwrap(), [0x311, 0x320]);
    }

    #[test]
    fn rejects_truncated_or_overlapping_executable_tables_and_data() {
        let bytes = pe();
        for length in [0, 2, 63, 0x83, 0x97, 0x177, 0x1c7, 0x3ff] {
            assert!(overlay_candidates(&bytes[..length]).is_err());
        }
        let mut bytes = pe();
        set32(&mut bytes, 0x1b4, 0x280);
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = pe();
        set32(&mut bytes, 0x18c, 0x100);
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = ne();
        set16(&mut bytes, 0x120, u16::MAX);
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = ne();
        set16(&mut bytes, 0xd4, u16::MAX);
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = ne();
        set16(&mut bytes, 0xc8, 17);
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = ne();
        bytes[0xe0] = 2;
        assert!(overlay_candidates(&bytes).is_err());
        let mut bytes = ne();
        set16(&mut bytes, 0xcc, 2);
        assert!(overlay_candidates(&bytes).is_err());
    }
}
