use std::{fs::File, io::Read, path::Path};

use mmd_anim_format::{PmdParsedModel, PmxParsedModel, pmx};

pub(crate) const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;

// Check lengths before the shared codec allocates geometry or legacy PMD arrays.
// This pass retains no model data; decoding stays in mmd-anim-format.
pub(crate) fn read_source(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_SOURCE_BYTES {
        return Err(std::io::Error::other("文件超过 512 MiB 解析上限"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(std::io::Error::other("文件读取期间超过 512 MiB 解析上限"));
    }
    Ok(bytes)
}

pub(crate) fn parse_pmx_model(bytes: &[u8]) -> Result<PmxParsedModel, String> {
    validate_pmx(bytes)?;
    mmd_anim_format::parse_pmx_model(bytes).map_err(|error| error.to_string())
}

pub(crate) fn parse_pmd_model(bytes: &[u8]) -> Result<PmdParsedModel, String> {
    validate_pmd(bytes)?;
    mmd_anim_format::parse_pmd_model(bytes).map_err(|error| error.to_string())
}

pub(crate) fn is_model_path(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("pmx") || value.eq_ignore_ascii_case("pmd"))
}

struct Layout<'a> { bytes: &'a [u8], position: usize }

impl<'a> Layout<'a> {
    fn remaining(&self) -> usize { self.bytes.len().saturating_sub(self.position) }
    fn skip(&mut self, size: usize) -> Result<(), String> {
        self.position = self.position.checked_add(size).filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "模型数据截断或区段长度无效".to_owned())?;
        Ok(())
    }
    fn u8(&mut self) -> Result<usize, String> { let start = self.position; self.skip(1)?; Ok(self.bytes[start] as usize) }
    fn u16(&mut self) -> Result<usize, String> {
        let start = self.position; self.skip(2)?;
        Ok(u16::from_le_bytes(self.bytes[start..start + 2].try_into().unwrap()) as usize)
    }
    fn u32(&mut self) -> Result<usize, String> {
        let start = self.position; self.skip(4)?;
        Ok(u32::from_le_bytes(self.bytes[start..start + 4].try_into().unwrap()) as usize)
    }
    fn require(&self, count: usize, stride: usize, maximum: usize) -> Result<(), String> {
        if count > maximum || count.checked_mul(stride).is_none_or(|size| size > self.remaining()) {
            return Err("模型区段数量超过上限或可用文件长度".to_owned());
        }
        Ok(())
    }
    fn array(&mut self, count: usize, stride: usize, maximum: usize) -> Result<(), String> {
        self.require(count, stride, maximum)?; self.skip(count * stride)
    }
    fn text(&mut self) -> Result<(), String> {
        let length = self.u32()?;
        if length > 16 * 1024 * 1024 { return Err("模型文本长度超过 16 MiB 上限".to_owned()); }
        self.skip(length)
    }
    fn count(&mut self, stride: usize, maximum: usize) -> Result<usize, String> {
        let count = self.u32()?; self.require(count, stride, maximum)?; Ok(count)
    }
}

pub(crate) fn validate_pmx(bytes: &[u8]) -> Result<(), String> {
    let (header, position) = pmx::read_header(bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES || header.extra_uv_count > 4 {
        return Err("PMX 文件或追加 UV 数量超过支持范围".to_owned());
    }
    for size in [header.vertex_index_size, header.texture_index_size, header.material_index_size,
        header.bone_index_size, header.morph_index_size, header.rigidbody_index_size] {
        if ![1, 2, 4].contains(&size) { return Err("PMX 索引宽度必须是 1、2 或 4".to_owned()); }
    }
    let mut r = Layout { bytes, position };
    for _ in 0..4 { r.text()?; }
    let vertex_section = r.position;
    r.count(38 + header.extra_uv_count as usize * 16, 1_000_000)?;
    r.position = pmx::skip_vertices(bytes, &header, vertex_section).map_err(|error| error.to_string())?;
    let indices = r.count(header.vertex_index_size as usize, 12_000_000)?;
    if indices % 3 != 0 { return Err("PMX 索引数量不是完整三角形".to_owned()); }
    r.skip(indices * header.vertex_index_size as usize)?;
    let textures = r.count(4, 65_536)?;
    for _ in 0..textures { r.text()?; }
    let materials = r.count(80, 8_192)?;
    for _ in 0..materials {
        r.text()?; r.text()?; r.skip(65 + header.texture_index_size as usize * 2)?;
        r.skip(1)?;
        let shared = r.u8()?;
        r.skip(if shared == 0 { header.texture_index_size as usize } else { 1 })?;
        r.text()?;
        let count = r.u32()?;
        if count > indices || count % 3 != 0 { return Err("PMX 材质三角形数量无效".to_owned()); }
    }
    r.count(8 + 12 + header.bone_index_size as usize + 4 + 2, 16_384)?;
    // The PMX codec already validates minimum record bytes for later sections.
    Ok(())
}

fn physics_tail(bytes: &[u8], start: usize) -> bool {
    let read = |position: usize| bytes.get(position..position.checked_add(4)?)
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()) as usize);
    let Some(joints) = read(start).and_then(|count| count.checked_mul(83))
        .and_then(|size| start.checked_add(4)?.checked_add(size)) else { return false; };
    joints == bytes.len() || read(joints).and_then(|count| count.checked_mul(124))
        .and_then(|size| joints.checked_add(4)?.checked_add(size)) == Some(bytes.len())
}

fn validate_pmd(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > MAX_SOURCE_BYTES || bytes.get(..3) != Some(b"Pmd") {
        return Err("PMD 文件头或大小无效".to_owned());
    }
    let mut r = Layout { bytes, position: 3 };
    r.skip(4 + 20 + 256)?;
    let vertices = r.count(38, 1_000_000)?; r.skip(vertices * 38)?;
    let indices = r.count(2, 12_000_000)?;
    if indices % 3 != 0 { return Err("PMD 索引数量不是完整三角形".to_owned()); }
    r.skip(indices * 2)?;
    let materials = r.count(70, 8_192)?;
    for _ in 0..materials {
        r.skip(46)?;
        let count = r.u32()?;
        if count > indices || count % 3 != 0 { return Err("PMD 材质三角形数量无效".to_owned()); }
        r.skip(20)?;
    }
    let bones = r.u16()?; r.array(bones, 39, 16_384)?;
    let ik = r.u16()?; r.require(ik, 11, 16_384)?;
    for _ in 0..ik { r.skip(4)?; let links = r.u8()?; r.skip(6)?; r.array(links, 2, 255)?; }
    let morphs = r.u16()?; r.require(morphs, 25, 65_535)?;
    for _ in 0..morphs {
        r.skip(20)?; let offsets = r.count(16, 12_000_000)?; r.skip(1)?; r.array(offsets, 16, 12_000_000)?;
    }
    if r.remaining() > 0 { let count = r.u8()?; r.array(count, 2, 255)?; }
    let display_names = if r.remaining() > 0 {
        let names = r.u8()?; r.array(names, 50, 255)?;
        if r.remaining() >= 4 { let count = r.count(3, 1_000_000)?; r.skip(count * 3)?; }
        names
    } else { 0 };
    if r.remaining() > 0 && r.u8()? != 0 {
        r.skip(276)?; r.array(bones, 20, 16_384)?;
        r.array(morphs.saturating_sub(1), 20, 65_535)?; r.array(display_names, 50, 255)?;
    }
    if r.remaining() >= 1000 && !physics_tail(bytes, r.position) { r.skip(1000)?; }
    if r.remaining() >= 4 { let count = r.count(83, 65_536)?; r.skip(count * 83)?; }
    if r.remaining() >= 4 { let count = r.count(124, 65_536)?; r.skip(count * 124)?; }
    Ok(())
}
