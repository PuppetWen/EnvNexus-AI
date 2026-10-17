//! Data-only extraction of official historical CPython distributions.
//!
//! Never launches Burn, msiexec, MSI custom actions, or system CRT installers.
//! Burn locations come from the PE .wixburn section and its manifest, rather
//! than scanning executable bytes for coincidental cabinet signatures.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read},
    ops::Range,
    path::{Path, PathBuf},
};

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::error::{AppError, AppResult};

#[path = "python_cab.rs"]
mod python_cab;

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL: u64 = 20 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const BURN_NAMESPACE: &str = "http://schemas.microsoft.com/wix/2008/Burn";

fn invalid(message: impl Into<String>) -> AppError {
    AppError::InvalidSource(format!("Python 历史发行包：{}", message.into()))
}

fn limited_bytes(reader: impl Read, limit: u64) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("文件超过解压大小限制"));
    }
    Ok(bytes)
}

pub(super) fn extract(archive: &Path, destination: &Path) -> AppResult<()> {
    let bytes = limited_bytes(File::open(archive)?, MAX_ARCHIVE)?;
    let mut output = ExtractionOutput::new(destination);
    if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        extract_msi(bytes, &mut output)?;
    } else if bytes.starts_with(b"MZ") {
        extract_burn(&bytes, &mut output)?;
    } else {
        return Err(invalid("仅支持官方 MSI 或 WiX Burn 完整安装包"));
    }
    if !destination.join("python.exe").is_file() || !destination.join("Lib/os.py").is_file() {
        return Err(invalid("没有提取到完整解释器及标准库"));
    }
    Ok(())
}

fn u16_at(bytes: &[u8], offset: usize) -> AppResult<u16> {
    let raw = bytes
        .get(offset..offset.checked_add(2).ok_or_else(|| invalid("偏移溢出"))?)
        .ok_or_else(|| invalid("文件头被截断"))?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> AppResult<u32> {
    let raw = bytes
        .get(offset..offset.checked_add(4).ok_or_else(|| invalid("偏移溢出"))?)
        .ok_or_else(|| invalid("文件头被截断"))?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn checked_range(start: usize, size: usize, length: usize) -> AppResult<Range<usize>> {
    let end = start
        .checked_add(size)
        .filter(|end| *end <= length)
        .ok_or_else(|| invalid("容器超出了发行包边界"))?;
    Ok(start..end)
}

fn burn_containers(bytes: &[u8]) -> AppResult<Vec<Range<usize>>> {
    if bytes.get(..2) != Some(b"MZ") {
        return Err(invalid("缺少 PE 文件头"));
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe.checked_add(4).ok_or_else(|| invalid("PE 偏移溢出"))?) != Some(b"PE\0\0")
    {
        return Err(invalid("PE 签名无效"));
    }
    let sections = u16_at(bytes, pe + 6)? as usize;
    let optional_size = u16_at(bytes, pe + 20)? as usize;
    if sections == 0 || sections > 96 {
        return Err(invalid("PE 分节数量无效"));
    }
    let optional = pe.checked_add(24).ok_or_else(|| invalid("PE 偏移溢出"))?;
    let security = match u16_at(bytes, optional)? {
        0x10b if optional_size >= 136 => optional + 128,
        0x20b if optional_size >= 152 => optional + 144,
        _ => return Err(invalid("PE 可选头无效")),
    };
    let signature = u32_at(bytes, security)? as usize;
    let signature_size = u32_at(bytes, security + 4)? as usize;
    let table = optional
        .checked_add(optional_size)
        .ok_or_else(|| invalid("PE 偏移溢出"))?;
    checked_range(table, sections * 40, bytes.len())?;
    let mut header = None;
    for index in 0..sections {
        let section = table + index * 40;
        if &bytes[section..section + 8] == b".wixburn" {
            if header.is_some() {
                return Err(invalid("重复的 Burn 分节"));
            }
            let size = u32_at(bytes, section + 16)? as usize;
            let offset = u32_at(bytes, section + 20)? as usize;
            header = Some(&bytes[checked_range(offset, size, bytes.len())?]);
        }
    }
    let header = header.ok_or_else(|| invalid("该 EXE 不是受支持的 WiX Burn 安装包"))?;
    if u32_at(header, 0)? != 0x00f1_4300 || u32_at(header, 4)? != 2 || u32_at(header, 40)? != 1 {
        return Err(invalid("不支持的 Burn 容器格式"));
    }
    let stub = u32_at(header, 24)? as usize;
    let original_signature = u32_at(header, 32)? as usize;
    let original_signature_size = u32_at(header, 36)? as usize;
    let count = u32_at(header, 44)? as usize;
    if !(2..=16).contains(&count) {
        return Err(invalid("完整安装包缺少附加容器"));
    }
    checked_range(48, count * 4, header.len())?;
    let ux_size = u32_at(header, 48)? as usize;
    let ux = checked_range(stub, ux_size, bytes.len())?;
    let mut offset = if original_signature != 0 {
        checked_range(original_signature, original_signature_size, bytes.len())?.end
    } else if signature != 0 {
        checked_range(signature, signature_size, bytes.len())?.end
    } else {
        ux.end
    };
    if offset < ux.end {
        return Err(invalid("Burn 签名与容器边界重叠"));
    }
    let mut ranges = vec![ux];
    for index in 1..count {
        let size = u32_at(header, 48 + index * 4)? as usize;
        let range = checked_range(offset, size, bytes.len())?;
        offset = range.end;
        ranges.push(range);
    }
    for range in &ranges {
        if bytes.get(range.start..range.start.saturating_add(4)) != Some(b"MSCF") {
            return Err(invalid("Burn 声明的容器缺少 CAB 签名"));
        }
    }
    Ok(ranges)
}

#[derive(Debug)]
struct BurnContainer {
    index: usize,
    size: u64,
    hash: String,
}
#[derive(Debug, Clone)]
struct BurnPayload {
    id: String,
    name: String,
    source: String,
    container: String,
    size: u64,
    hash: String,
}

fn attributes(
    element: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
) -> AppResult<HashMap<String, String>> {
    let mut result = HashMap::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| invalid(format!("XML 属性无效：{error}")))?;
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|_| invalid("XML 属性编码无效"))?
            .to_string();
        let value = attribute
            .decode_and_unescape_value(reader.decoder())
            .map_err(|error| invalid(format!("XML 属性无效：{error}")))?
            .into_owned();
        if result.insert(key, value).is_some() {
            return Err(invalid("XML 属性重复"));
        }
    }
    Ok(result)
}

fn required<'a>(attributes: &'a HashMap<String, String>, key: &str) -> AppResult<&'a str> {
    attributes
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("清单缺少 {key}")))
}

fn manifest(xml: &[u8]) -> AppResult<(HashMap<String, BurnContainer>, Vec<BurnPayload>)> {
    let mut reader = Reader::from_reader(xml);
    let mut depth = 0usize;
    let mut containers = HashMap::new();
    let mut payloads = Vec::new();
    let mut root_found = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| invalid(format!("Burn XML 无效：{error}")))?;
        let is_start = matches!(event, Event::Start(_));
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let name = element.local_name();
                let attributes = attributes(element, &reader)?;
                if depth == 0 {
                    if root_found
                        || name.as_ref() != b"BurnManifest"
                        || required(&attributes, "xmlns")? != BURN_NAMESPACE
                    {
                        return Err(invalid("Burn 清单根节点无效"));
                    }
                    root_found = true;
                } else if depth == 1 && name.as_ref() == b"Container" {
                    if required(&attributes, "Attached")? != "yes" {
                        return Err(invalid("不接受安装包外部容器"));
                    }
                    let id = required(&attributes, "Id")?.to_string();
                    let container = BurnContainer {
                        index: required(&attributes, "AttachedIndex")?
                            .parse()
                            .map_err(|_| invalid("容器索引无效"))?,
                        size: required(&attributes, "FileSize")?
                            .parse()
                            .map_err(|_| invalid("容器大小无效"))?,
                        hash: required(&attributes, "Hash")?.to_string(),
                    };
                    if containers.insert(id, container).is_some() {
                        return Err(invalid("容器 ID 重复"));
                    }
                } else if depth == 1
                    && name.as_ref() == b"Payload"
                    && attributes.get("Packaging").map(String::as_str) == Some("embedded")
                {
                    payloads.push(BurnPayload {
                        id: required(&attributes, "Id")?.to_string(),
                        name: required(&attributes, "FilePath")?.to_string(),
                        source: required(&attributes, "SourcePath")?.to_string(),
                        container: required(&attributes, "Container")?.to_string(),
                        size: required(&attributes, "FileSize")?
                            .parse()
                            .map_err(|_| invalid("发行包大小无效"))?,
                        hash: required(&attributes, "Hash")?.to_string(),
                    });
                    if payloads.len() > 1024 {
                        return Err(invalid("发行包数量超过限制"));
                    }
                }
                if is_start {
                    depth += 1;
                    if depth > 64 {
                        return Err(invalid("XML 嵌套超过限制"));
                    }
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("XML 节点不匹配"))?;
            }
            Event::DocType(_) => return Err(invalid("不接受 XML DTD")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_found || depth != 0 || containers.is_empty() {
        return Err(invalid("Burn 清单不完整"));
    }
    Ok((containers, payloads))
}

fn verify_manifest_hash(bytes: &[u8], size: u64, expected: &str) -> AppResult<()> {
    if bytes.len() as u64 != size || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("清单大小或校验值无效"));
    }
    let actual = match expected.len() {
        40 => hex::encode(Sha1::digest(bytes)),
        64 => hex::encode(Sha256::digest(bytes)),
        128 => hex::encode(Sha512::digest(bytes)),
        _ => return Err(invalid("不支持的清单校验算法")),
    };
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(invalid("内置容器或组件校验失败"));
    }
    Ok(())
}

type ReadCabinet<'a> = cab::Cabinet<Cursor<&'a [u8]>>;
type CabinetEntries = Vec<(String, u64)>;

fn cabinet_files(bytes: &[u8]) -> AppResult<(ReadCabinet<'_>, CabinetEntries)> {
    let cabinet = cab::Cabinet::new(Cursor::new(bytes))
        .map_err(|error| invalid(format!("CAB 无效：{error}")))?;
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    let mut size = 0u64;
    for folder in cabinet.folder_entries() {
        for file in folder.file_entries() {
            if !names.insert(file.name().to_string()) {
                return Err(invalid("CAB 文件 ID 重复"));
            }
            size = size
                .checked_add(file.uncompressed_size() as u64)
                .ok_or_else(|| invalid("CAB 大小溢出"))?;
            if entries.len() >= MAX_FILES || size > MAX_TOTAL {
                return Err(invalid("CAB 解压数量或大小超过限制"));
            }
            entries.push((file.name().to_string(), file.uncompressed_size() as u64));
        }
    }
    Ok((cabinet, entries))
}

fn extract_burn(bytes: &[u8], output: &mut ExtractionOutput<'_>) -> AppResult<()> {
    let ranges = burn_containers(bytes)?;
    let (mut ux, _) = cabinet_files(&bytes[ranges[0].clone()])?;
    let xml = limited_bytes(
        ux.read_file("0")
            .map_err(|error| invalid(format!("Burn 缺少清单：{error}")))?,
        4 * 1024 * 1024,
    )?;
    let (containers, payloads) = manifest(&xml)?;
    let wanted = [
        "core.msi",
        "exe.msi",
        "lib.msi",
        "dev.msi",
        "tcltk.msi",
        "tools.msi",
        "test.msi",
        "doc.msi",
        "pip.msi",
    ];
    let mut selected = BTreeMap::<String, BurnPayload>::new();
    for payload in payloads {
        if !wanted.contains(&payload.name.as_str()) {
            continue;
        }
        single_name(&payload.source, false)?;
        if let Some(old) = selected.get(&payload.name) {
            if old.size != payload.size || !old.hash.eq_ignore_ascii_case(&payload.hash) {
                return Err(invalid("同名组件内容冲突"));
            }
            if !payload.id.ends_with("_JustForMe") {
                continue;
            }
        }
        selected.insert(payload.name.clone(), payload);
    }
    for name in ["core.msi", "exe.msi", "lib.msi", "dev.msi", "tcltk.msi"] {
        if !selected.contains_key(name) {
            return Err(invalid(format!(
                "完整 EXE 缺少内置组件 {name}，不会联网替换发行包"
            )));
        }
    }
    let mut checked = HashSet::new();
    for payload in selected.values() {
        let container = containers
            .get(&payload.container)
            .ok_or_else(|| invalid("组件引用了未知容器"))?;
        if container.index == 0 {
            return Err(invalid("运行时组件不能来自 UX 容器"));
        }
        let range = ranges
            .get(container.index)
            .ok_or_else(|| invalid("容器索引超出边界"))?;
        let container_bytes = &bytes[range.clone()];
        if checked.insert(&payload.container) {
            verify_manifest_hash(container_bytes, container.size, &container.hash)?;
        }
        let (mut cabinet, _) = cabinet_files(container_bytes)?;
        let msi = limited_bytes(
            cabinet
                .read_file(&payload.source)
                .map_err(|error| invalid(format!("缺少内置组件：{error}")))?,
            MAX_ARCHIVE,
        )?;
        verify_manifest_hash(&msi, payload.size, &payload.hash)?;
        extract_msi(msi, output)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Directory {
    parent: String,
    name: String,
}

fn single_name(value: &str, alias: bool) -> AppResult<&str> {
    let mut parts = value.split('|');
    let short = parts.next().unwrap_or(value);
    let value = parts.next().unwrap_or(short);
    if parts.next().is_some() {
        return Err(invalid("MSI 文件名包含多余的长短名称分隔符"));
    }
    if short != value {
        single_name(short, alias)?;
    }
    if alias && value == "." {
        return Ok("");
    }
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || value.chars().any(|ch| {
            ch < ' ' || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(invalid(format!("文件或目录名称不安全：{value}")));
    }
    let base = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&base.as_str())
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(invalid("发行包包含 Windows 设备文件名"));
    }
    Ok(value)
}

fn directory_path(
    id: &str,
    directories: &HashMap<String, Directory>,
    visiting: &mut HashSet<String>,
) -> AppResult<PathBuf> {
    if id == "TARGETDIR" {
        return Ok(PathBuf::new());
    }
    if visiting.len() >= 128 || !visiting.insert(id.to_string()) {
        return Err(invalid("MSI 目录树出现循环或过深"));
    }
    let directory = directories
        .get(id)
        .ok_or_else(|| invalid(format!("未知 MSI 目录：{id}")))?;
    if directory.parent.is_empty() || directory.parent == id {
        return Err(invalid("MSI 包含额外根目录"));
    }
    let mut path = directory_path(&directory.parent, directories, visiting)?;
    let name = if let Some((target, source)) = directory.name.split_once(':') {
        single_name(source, true)?;
        single_name(target, true)?
    } else {
        single_name(&directory.name, true)?
    };
    if !name.is_empty() {
        path.push(name);
    }
    visiting.remove(id);
    Ok(path)
}

fn system_directory(id: &str, directories: &HashMap<String, Directory>) -> bool {
    let mut next = id;
    for _ in 0..=128 {
        if [
            "WindowsFolder",
            "SystemFolder",
            "System64Folder",
            "ProgramFilesFolder",
            "ProgramFiles64Folder",
            "CommonFilesFolder",
            "ProgramMenuFolder",
            "LAUNCHERDIR",
        ]
        .iter()
        .any(|prefix| {
            next == *prefix
                || next.starts_with(&format!("{prefix}."))
                || next.starts_with(&format!("{prefix}_"))
        }) {
            return true;
        }
        let Some(directory) = directories.get(next) else {
            return false;
        };
        if next == "TARGETDIR" || directory.parent.is_empty() {
            return false;
        }
        next = &directory.parent;
    }
    false
}

struct ExtractionOutput<'a> {
    root: &'a Path,
    paths: HashSet<String>,
    size: u64,
}
impl<'a> ExtractionOutput<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            paths: HashSet::new(),
            size: 0,
        }
    }
    fn reserve(&mut self, path: &Path, size: u64) -> AppResult<()> {
        let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        self.size = self
            .size
            .checked_add(size)
            .ok_or_else(|| invalid("总解压大小溢出"))?;
        if self.paths.len() >= MAX_FILES || self.size > MAX_TOTAL || size > MAX_FILE {
            return Err(invalid("解压数量或大小超过限制"));
        }
        if !self.paths.insert(key) {
            return Err(invalid(format!("解压路径重复：{}", path.display())));
        }
        Ok(())
    }
    fn write(&self, relative: &Path, expected_size: u64, reader: impl Read) -> AppResult<()> {
        let path = self.root.join(relative);
        let relative_parent = relative
            .parent()
            .ok_or_else(|| invalid("解压路径缺少父目录"))?;
        let mut parent = self.root.to_path_buf();
        plain_directory(&parent)?;
        for component in relative_parent.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(invalid("解压路径包含非相对目录"));
            };
            parent.push(name);
            match fs::create_dir(&parent) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            plain_directory(&parent)?;
        }
        if !fs::canonicalize(&parent)?.starts_with(fs::canonicalize(self.root)?) {
            return Err(invalid("解压目录逃逸受管根目录"));
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let actual = std::io::copy(&mut reader.take(expected_size.saturating_add(1)), &mut file)?;
        if actual != expected_size {
            return Err(invalid("解压文件与 MSI 声明大小不符"));
        }
        Ok(())
    }
}

fn plain_directory(path: &Path) -> AppResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    let reparse = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        reparse || metadata.file_attributes() & 0x400 != 0
    };
    if reparse || !metadata.is_dir() {
        return Err(invalid("解压目录包含重解析点或非目录条目"));
    }
    Ok(())
}

fn text(row: &msi::Row, column: &str) -> AppResult<String> {
    row[column]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("MSI 列 {column} 不是字符串")))
}
fn number(row: &msi::Row, column: &str) -> AppResult<i32> {
    row[column]
        .as_int()
        .ok_or_else(|| invalid(format!("MSI 列 {column} 不是整数")))
}

fn extract_msi(bytes: Vec<u8>, output: &mut ExtractionOutput<'_>) -> AppResult<()> {
    let mut package = msi::Package::open(Cursor::new(bytes))
        .map_err(|error| invalid(format!("MSI 无效：{error}")))?;
    let mut directories = HashMap::new();
    for row in package.select_rows(msi::Select::table("Directory"))? {
        let id = text(&row, "Directory")?;
        let directory = Directory {
            parent: row["Directory_Parent"].as_str().unwrap_or("").to_string(),
            name: text(&row, "DefaultDir")?,
        };
        if directories.insert(id, directory).is_some() {
            return Err(invalid("MSI 目录 ID 重复"));
        }
    }
    if !directories.contains_key("TARGETDIR") {
        return Err(invalid("MSI 缺少 TARGETDIR"));
    }
    for id in directories.keys() {
        directory_path(id, &directories, &mut HashSet::new())?;
    }
    let mut shared_crt = HashSet::new();
    if package.has_table("FeatureComponents") {
        for row in package.select_rows(msi::Select::table("FeatureComponents"))? {
            if text(&row, "Feature_")?.eq_ignore_ascii_case("SharedCRT") {
                shared_crt.insert(text(&row, "Component_")?);
            }
        }
    }
    let mut components = HashMap::new();
    for row in package.select_rows(msi::Select::table("Component"))? {
        let id = text(&row, "Component")?;
        let directory = text(&row, "Directory_")?;
        let include = !shared_crt.contains(&id) && !system_directory(&directory, &directories);
        if components.insert(id, (directory, include)).is_some() {
            return Err(invalid("MSI 组件 ID 重复"));
        }
    }
    let mut files = HashMap::new();
    if package.has_table("File") {
        for row in package.select_rows(msi::Select::table("File"))? {
            let component = text(&row, "Component_")?;
            let (directory, include) = components
                .get(&component)
                .ok_or_else(|| invalid("MSI 文件引用了未知组件"))?;
            if !include {
                continue;
            }
            let name = text(&row, "FileName")?;
            let name = single_name(&name, false)?;
            let size = number(&row, "FileSize")?;
            let sequence = number(&row, "Sequence")?;
            if size < 0 || sequence <= 0 {
                return Err(invalid("MSI 文件大小或序号无效"));
            }
            let path = directory_path(directory, &directories, &mut HashSet::new())?.join(name);
            output.reserve(&path, size as u64)?;
            if files
                .insert(text(&row, "File")?, (path, size as u64, sequence))
                .is_some()
            {
                return Err(invalid("MSI 文件 ID 重复"));
            }
        }
    }
    if files.is_empty() {
        return Ok(());
    }
    let mut media = Vec::new();
    for row in package.select_rows(msi::Select::table("Media"))? {
        media.push((number(&row, "LastSequence")?, text(&row, "Cabinet")?));
    }
    media.sort_by_key(|(last, _)| *last);
    let mut previous = 0;
    let mut extracted = HashSet::new();
    for (last, cabinet) in media {
        if last <= previous {
            return Err(invalid("MSI 媒体序号重复或无效"));
        }
        let wanted = files
            .values()
            .any(|(_, _, seq)| *seq > previous && *seq <= last);
        if wanted {
            let stream_name = cabinet
                .strip_prefix('#')
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid("不接受 MSI 外部 CAB"))?;
            let cabinet_bytes = limited_bytes(package.read_stream(stream_name)?, MAX_ARCHIVE)?;
            let selected = files
                .iter()
                .filter(|(_, (_, _, seq))| *seq > previous && *seq <= last)
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>();
            python_cab::for_each_file(&cabinet_bytes, &selected, |id, content| {
                if let Some((path, expected, seq)) = files.get(id) {
                    if *seq <= previous
                        || *seq > last
                        || *expected != content.len() as u64
                        || !extracted.insert(id.to_string())
                    {
                        return Err(invalid("MSI 与 CAB 文件映射冲突"));
                    }
                    output.write(path, *expected, Cursor::new(content))?;
                }
                Ok(())
            })?;
        }
        previous = last;
    }
    if extracted.len() != files.len() {
        return Err(invalid("MSI 内置 CAB 缺少已声明文件"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use msi::{Column, Insert, Package, PackageType, Value};
    use std::io::Write;

    fn cabinet(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = cab::CabinetBuilder::new();
        let folder = builder.add_folder(cab::CompressionType::MsZip);
        for (name, _) in entries {
            folder.add_file(*name);
        }
        let mut writer = builder.build(Cursor::new(Vec::new())).unwrap();
        while let Some(mut file) = writer.next_file().unwrap() {
            let bytes = entries
                .iter()
                .find(|(name, _)| *name == file.file_name())
                .unwrap()
                .1;
            file.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn package(empty: bool, wrong_size: bool) -> Vec<u8> {
        let mut package = Package::create(PackageType::Installer, Cursor::new(Vec::new())).unwrap();
        package
            .create_table(
                "Directory",
                vec![
                    Column::build("Directory").primary_key().id_string(72),
                    Column::build("Directory_Parent").nullable().id_string(72),
                    Column::build("DefaultDir").string(255),
                ],
            )
            .unwrap();
        package
            .insert_rows(
                Insert::into("Directory")
                    .row(vec!["TARGETDIR".into(), Value::Null, "SourceDir".into()])
                    .row(vec!["Lib".into(), "TARGETDIR".into(), "LIB|Lib".into()])
                    .row(vec![
                        "WindowsFolder".into(),
                        "TARGETDIR".into(),
                        "Windows".into(),
                    ]),
            )
            .unwrap();
        package
            .create_table(
                "Component",
                vec![
                    Column::build("Component").primary_key().id_string(72),
                    Column::build("Directory_").id_string(72),
                ],
            )
            .unwrap();
        if !empty {
            package
                .insert_rows(
                    Insert::into("Component")
                        .row(vec!["exe".into(), "TARGETDIR".into()])
                        .row(vec!["lib".into(), "Lib".into()])
                        .row(vec!["privatecrt".into(), "TARGETDIR".into()])
                        .row(vec!["sharedcrt".into(), "WindowsFolder".into()]),
                )
                .unwrap();
            package
                .create_table(
                    "FeatureComponents",
                    vec![
                        Column::build("Feature_").primary_key().id_string(72),
                        Column::build("Component_").primary_key().id_string(72),
                    ],
                )
                .unwrap();
            package
                .insert_rows(
                    Insert::into("FeatureComponents")
                        .row(vec!["SharedCRT".into(), "sharedcrt".into()])
                        .row(vec!["PrivateCRT".into(), "privatecrt".into()]),
                )
                .unwrap();
            package
                .create_table(
                    "File",
                    vec![
                        Column::build("File").primary_key().id_string(72),
                        Column::build("Component_").id_string(72),
                        Column::build("FileName").string(255),
                        Column::build("FileSize").int32(),
                        Column::build("Sequence").int16(),
                    ],
                )
                .unwrap();
            package
                .insert_rows(
                    Insert::into("File")
                        .row(vec![
                            "python_exe".into(),
                            "exe".into(),
                            "PYTHON.EXE|python.exe".into(),
                            (if wrong_size { 7 } else { 6 }).into(),
                            1.into(),
                        ])
                        .row(vec![
                            "os_py".into(),
                            "lib".into(),
                            "os.py".into(),
                            6.into(),
                            2.into(),
                        ])
                        .row(vec![
                            "private_crt".into(),
                            "privatecrt".into(),
                            "msvcr90.dll".into(),
                            7.into(),
                            3.into(),
                        ])
                        .row(vec![
                            "shared_crt".into(),
                            "sharedcrt".into(),
                            "msvcr90.dll".into(),
                            6.into(),
                            4.into(),
                        ]),
                )
                .unwrap();
            package
                .create_table(
                    "Media",
                    vec![
                        Column::build("DiskId").primary_key().int16(),
                        Column::build("LastSequence").int16(),
                        Column::build("Cabinet").string(255),
                    ],
                )
                .unwrap();
            package
                .insert_rows(Insert::into("Media").row(vec![1.into(), 4.into(), "#python".into()]))
                .unwrap();
            package
                .write_stream("python")
                .unwrap()
                .write_all(&cabinet(&[
                    ("python_exe", b"python"),
                    ("os_py", b"stdlib"),
                    ("private_crt", b"private"),
                    ("shared_crt", b"shared"),
                ]))
                .unwrap();
        }
        package.into_inner().unwrap().into_inner()
    }

    fn bundle() -> Vec<u8> {
        let core = package(false, false);
        let empty = package(true, false);
        let components = [
            ("core", core.as_slice()),
            ("exe", empty.as_slice()),
            ("lib", empty.as_slice()),
            ("dev", empty.as_slice()),
            ("tcltk", empty.as_slice()),
        ];
        let names = (0..components.len())
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>();
        let entries = components
            .iter()
            .zip(&names)
            .map(|((_, bytes), name)| (name.as_str(), *bytes))
            .collect::<Vec<_>>();
        let attached = cabinet(&entries);
        let mut xml = format!(
            "<BurnManifest xmlns=\"{BURN_NAMESPACE}\"><UX/><Container Id=\"attached\" Attached=\"yes\" AttachedIndex=\"1\" FileSize=\"{}\" Hash=\"{}\"/>",
            attached.len(),
            hex::encode(Sha1::digest(&attached))
        );
        for ((name, bytes), source) in components.iter().zip(&names) {
            xml.push_str(&format!("<Payload Id=\"{name}_JustForMe\" FilePath=\"{name}.msi\" Packaging=\"embedded\" SourcePath=\"{source}\" Container=\"attached\" FileSize=\"{}\" Hash=\"{}\"/>",bytes.len(),hex::encode(Sha1::digest(bytes))));
        }
        xml.push_str("</BurnManifest>");
        let ux = cabinet(&[("0", xml.as_bytes())]);
        let mut bytes = vec![0; 512];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
        bytes[84..86].copy_from_slice(&224u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        let section = 312;
        bytes[section..section + 8].copy_from_slice(b".wixburn");
        bytes[section + 16..section + 20].copy_from_slice(&64u32.to_le_bytes());
        bytes[section + 20..section + 24].copy_from_slice(&384u32.to_le_bytes());
        for (offset, value) in [
            (0, 0x00f1_4300),
            (4, 2),
            (24, 512),
            (40, 1),
            (44, 2),
            (48, ux.len() as u32),
            (52, attached.len() as u32),
        ] {
            bytes[384 + offset..388 + offset].copy_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&ux);
        bytes.extend_from_slice(&attached);
        bytes
    }

    #[test]
    fn static_msi_extracts_runtime_and_private_crt_without_system_payloads() {
        let root = tempfile::tempdir().unwrap();
        extract_msi(
            package(false, false),
            &mut ExtractionOutput::new(root.path()),
        )
        .unwrap();
        assert_eq!(fs::read(root.path().join("python.exe")).unwrap(), b"python");
        assert_eq!(fs::read(root.path().join("Lib/os.py")).unwrap(), b"stdlib");
        assert_eq!(
            fs::read(root.path().join("msvcr90.dll")).unwrap(),
            b"private"
        );
        assert!(!root.path().join("Windows").exists());
    }

    #[test]
    fn static_burn_uses_section_manifest_and_verified_embedded_msis() {
        let root = tempfile::tempdir().unwrap();
        extract_burn(&bundle(), &mut ExtractionOutput::new(root.path())).unwrap();
        assert_eq!(fs::read(root.path().join("python.exe")).unwrap(), b"python");
    }

    #[test]
    fn rejects_fake_embedded_cabinet_and_truncated_burn_container() {
        assert!(burn_containers(b"MZ arbitrary MSCF fake cabinet").is_err());
        let mut bytes = bundle();
        bytes.pop();
        assert!(burn_containers(&bytes).is_err());
    }

    #[test]
    fn rejects_msi_file_size_mismatch_and_duplicate_extraction_paths() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            extract_msi(
                package(false, true),
                &mut ExtractionOutput::new(root.path())
            )
            .is_err()
        );
        let root = tempfile::tempdir().unwrap();
        let mut output = ExtractionOutput::new(root.path());
        output.reserve(Path::new("Lib/os.py"), 1).unwrap();
        assert!(output.reserve(Path::new("lib/OS.PY"), 1).is_err());
        assert!(output.reserve(Path::new("huge"), MAX_TOTAL + 1).is_err());
    }

    #[test]
    fn rejects_traversal_ads_devices_and_directory_cycles() {
        for name in [
            "..",
            r"..\outside",
            r"E:\outside",
            "file:stream",
            "NUL.txt",
            "COM1",
            "trailing.",
        ] {
            assert!(single_name(name, false).is_err(), "{name}");
        }
        let dirs = HashMap::from([
            (
                "a".into(),
                Directory {
                    parent: "b".into(),
                    name: "a".into(),
                },
            ),
            (
                "b".into(),
                Directory {
                    parent: "a".into(),
                    name: "b".into(),
                },
            ),
        ]);
        assert!(directory_path("a", &dirs, &mut HashSet::new()).is_err());
        assert!(verify_manifest_hash(b"changed", 7, &"a".repeat(40)).is_err());
    }

    #[test]
    #[ignore = "reads official historical installers from ENVNEXUS_PYTHON_ARCHIVES and only extracts data"]
    fn official_historical_python_archives_extract_without_running_installers() {
        let directory =
            PathBuf::from(std::env::var_os("ENVNEXUS_PYTHON_ARCHIVES").expect("archive directory"));
        for name in [
            "python-2.7.18.amd64.msi",
            "python-3.4.4.amd64.msi",
            "python-3.10.11-amd64.exe",
        ] {
            let target = tempfile::tempdir_in(&directory).unwrap();
            extract(&directory.join(name), target.path()).unwrap();
            assert!(target.path().join("python.exe").is_file());
            assert!(target.path().join("include/Python.h").is_file());
            assert!(!target.path().join("Windows").exists());
        }
    }
}
