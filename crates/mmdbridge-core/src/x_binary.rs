use std::collections::HashSet;

use encoding_rs::SHIFT_JIS;
use mmd_anim_format::{
    AccessoryParsedManifest,
    xfile::{AccessoryDiagnostic, AccessoryMaterial, AccessoryMeshSummary, AccessoryVertexColor},
};

const TOKEN_NAME: u16 = 1;
const TOKEN_STRING: u16 = 2;
const TOKEN_INTEGER: u16 = 3;
const TOKEN_GUID: u16 = 5;
const TOKEN_INTEGER_LIST: u16 = 6;
const TOKEN_FLOAT_LIST: u16 = 7;
const TOKEN_OBRACE: u16 = 10;
const TOKEN_CBRACE: u16 = 11;
const TOKEN_COMMA: u16 = 19;
const TOKEN_SEMICOLON: u16 = 20;
const TOKEN_TEMPLATE: u16 = 31;
const MAX_NAME_BYTES: usize = 1024 * 1024;
const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
enum Token {
    Name(String),
    String(String),
    Integer(u32),
    Integers(Vec<u32>),
    Floats(Vec<f32>),
    Guid,
    OpenBrace,
    CloseBrace,
    Template,
    Separator,
    Other,
}

#[derive(Debug)]
enum ValueField {
    Integers(Vec<u32>),
    Floats(Vec<f32>),
    String(String),
}

#[derive(Debug)]
struct Node {
    kind: String,
    name: Option<String>,
    fields: Vec<ValueField>,
    children: Vec<Node>,
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
    float_width: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], float_width: usize) -> Self {
        Self {
            data,
            offset: 16,
            float_width,
        }
    }

    fn read_token(&mut self) -> Result<Option<Token>, String> {
        if self.offset == self.data.len() {
            return Ok(None);
        }
        let token = self.read_u16()?;
        match token {
            TOKEN_NAME => {
                let length = self.read_count(MAX_NAME_BYTES, "name")?;
                let bytes = self.take(length)?;
                let name = std::str::from_utf8(bytes)
                    .map_err(|_| "X 二进制对象名包含非 ASCII 数据".to_owned())?
                    .to_owned();
                Ok(Some(Token::Name(name)))
            }
            TOKEN_STRING => {
                let length = self.read_count(MAX_STRING_BYTES, "string")?;
                let bytes = self.take(length)?;
                let terminator = self.read_u16()?;
                if terminator != TOKEN_COMMA && terminator != TOKEN_SEMICOLON {
                    return Err("X 二进制字符串缺少逗号或分号终止符".to_owned());
                }
                let (decoded, _, _) = SHIFT_JIS.decode(bytes);
                Ok(Some(Token::String(decoded.into_owned())))
            }
            TOKEN_INTEGER => Ok(Some(Token::Integer(self.read_u32()?))),
            TOKEN_GUID => {
                self.take(16)?;
                Ok(Some(Token::Guid))
            }
            TOKEN_INTEGER_LIST => {
                let count = self.read_count(self.data.len() / 4, "integer list")?;
                let byte_count = count
                    .checked_mul(4)
                    .ok_or_else(|| "X 二进制整数列表长度溢出".to_owned())?;
                let bytes = self.take(byte_count)?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|_| "X 二进制整数列表内存不足".to_owned())?;
                for chunk in bytes.chunks_exact(4) {
                    values.push(u32::from_le_bytes(
                        chunk.try_into().expect("four-byte chunk"),
                    ));
                }
                Ok(Some(Token::Integers(values)))
            }
            TOKEN_FLOAT_LIST => {
                let count = self.read_count(self.data.len() / self.float_width, "float list")?;
                let byte_count = count
                    .checked_mul(self.float_width)
                    .ok_or_else(|| "X 二进制浮点列表长度溢出".to_owned())?;
                let bytes = self.take(byte_count)?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|_| "X 二进制浮点列表内存不足".to_owned())?;
                for chunk in bytes.chunks_exact(self.float_width) {
                    let value = if self.float_width == 4 {
                        f32::from_le_bytes(chunk.try_into().expect("four-byte float"))
                    } else {
                        f64::from_le_bytes(chunk.try_into().expect("eight-byte float")) as f32
                    };
                    if !value.is_finite() {
                        return Err("X 二进制浮点列表包含无效坐标或参数".to_owned());
                    }
                    values.push(value);
                }
                Ok(Some(Token::Floats(values)))
            }
            TOKEN_OBRACE => Ok(Some(Token::OpenBrace)),
            TOKEN_CBRACE => Ok(Some(Token::CloseBrace)),
            TOKEN_TEMPLATE => Ok(Some(Token::Template)),
            TOKEN_COMMA | TOKEN_SEMICOLON => Ok(Some(Token::Separator)),
            _ => Ok(Some(Token::Other)),
        }
    }

    fn read_count(&mut self, maximum: usize, label: &str) -> Result<usize, String> {
        let count = usize::try_from(self.read_u32()?)
            .map_err(|_| format!("X 二进制 {label} 数量超出平台范围"))?;
        if count > maximum {
            return Err(format!("X 二进制 {label} 数量超出文件长度"));
        }
        Ok(count)
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes(bytes.try_into().expect("two-byte word")))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().expect("four-byte dword"),
        ))
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| "X 二进制偏移溢出".to_owned())?;
        let bytes = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| "X 二进制文件意外结束".to_owned())?;
        self.offset = end;
        Ok(bytes)
    }

    fn skip_template(&mut self) -> Result<(), String> {
        let mut opened = false;
        let mut depth = 0usize;
        while let Some(token) = self.read_token()? {
            match token {
                Token::OpenBrace => {
                    opened = true;
                    depth += 1;
                }
                Token::CloseBrace if opened => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err("X 二进制模板定义不完整".to_owned())
    }

    fn parse_document(&mut self) -> Result<Vec<Node>, String> {
        let mut roots = Vec::new();
        while let Some(token) = self.read_token()? {
            match token {
                Token::Template => self.skip_template()?,
                Token::Name(kind) => roots.push(self.parse_node(kind)?),
                Token::OpenBrace => self.skip_reference(1)?,
                Token::CloseBrace => return Err("X 二进制对象括号不匹配".to_owned()),
                _ => {}
            }
        }
        Ok(roots)
    }

    fn parse_node(&mut self, kind: String) -> Result<Node, String> {
        let name = match self.read_token()? {
            Some(Token::OpenBrace) => None,
            Some(Token::Name(name)) => match self.read_token()? {
                Some(Token::OpenBrace) => Some(name),
                _ => return Err(format!("X 二进制 {kind} 对象缺少左大括号")),
            },
            _ => return Err(format!("X 二进制 {kind} 对象头无效")),
        };

        let mut node = Node {
            kind,
            name,
            fields: Vec::new(),
            children: Vec::new(),
        };
        let mut token = self.read_token()?;
        if matches!(token, Some(Token::Guid)) {
            token = self.read_token()?;
        }
        loop {
            match token {
                Some(Token::CloseBrace) => return Ok(node),
                Some(Token::Name(kind)) => node.children.push(self.parse_node(kind)?),
                Some(Token::OpenBrace) => self.skip_reference(1)?,
                Some(Token::Integer(value)) => node.push_integers([value])?,
                Some(Token::Integers(values)) => node.push_integers(values)?,
                Some(Token::Floats(values)) => node.push_floats(values)?,
                Some(Token::String(value)) => node.fields.push(ValueField::String(value)),
                Some(Token::Template) => return Err("X 二进制对象中不应嵌入模板定义".to_owned()),
                Some(Token::Guid | Token::Separator | Token::Other) => {}
                None => return Err(format!("X 二进制 {} 对象未闭合", node.kind)),
            }
            token = self.read_token()?;
        }
    }

    fn skip_reference(&mut self, mut depth: usize) -> Result<(), String> {
        while depth > 0 {
            match self.read_token()? {
                Some(Token::OpenBrace) => depth += 1,
                Some(Token::CloseBrace) => depth -= 1,
                Some(_) => {}
                None => return Err("X 二进制对象引用未闭合".to_owned()),
            }
        }
        Ok(())
    }
}

impl Node {
    fn push_integers(&mut self, values: impl IntoIterator<Item = u32>) -> Result<(), String> {
        let values = values.into_iter();
        if let Some(ValueField::Integers(current)) = self.fields.last_mut() {
            current
                .try_reserve(values.size_hint().0)
                .map_err(|_| "X 二进制整数列表内存不足".to_owned())?;
            current.extend(values);
        } else {
            let mut current = Vec::new();
            current
                .try_reserve(values.size_hint().0)
                .map_err(|_| "X 二进制整数列表内存不足".to_owned())?;
            current.extend(values);
            self.fields.push(ValueField::Integers(current));
        }
        Ok(())
    }

    fn push_floats(&mut self, values: Vec<f32>) -> Result<(), String> {
        if let Some(ValueField::Floats(current)) = self.fields.last_mut() {
            current
                .try_reserve(values.len())
                .map_err(|_| "X 二进制浮点列表内存不足".to_owned())?;
            current.extend(values);
        } else {
            self.fields.push(ValueField::Floats(values));
        }
        Ok(())
    }
}

struct ValueCursor<'a> {
    fields: &'a [ValueField],
    field_index: usize,
    value_index: usize,
}

impl<'a> ValueCursor<'a> {
    fn new(fields: &'a [ValueField]) -> Self {
        Self {
            fields,
            field_index: 0,
            value_index: 0,
        }
    }

    fn next_integer(&mut self, label: &str) -> Result<u32, String> {
        self.advance_empty();
        match self.fields.get(self.field_index) {
            Some(ValueField::Integers(values)) => {
                let value = values
                    .get(self.value_index)
                    .copied()
                    .ok_or_else(|| format!("X 二进制 {label} 数据不完整"))?;
                self.value_index += 1;
                Ok(value)
            }
            Some(ValueField::Floats(_)) => Err(format!("X 二进制 {label} 类型错误：预期整数")),
            Some(ValueField::String(_)) => Err(format!("X 二进制 {label} 类型错误：预期整数")),
            None => Err(format!("X 二进制 {label} 数据不完整")),
        }
    }

    fn next_float(&mut self, label: &str) -> Result<f32, String> {
        self.advance_empty();
        match self.fields.get(self.field_index) {
            Some(ValueField::Floats(values)) => {
                let value = values
                    .get(self.value_index)
                    .copied()
                    .ok_or_else(|| format!("X 二进制 {label} 数据不完整"))?;
                self.value_index += 1;
                Ok(value)
            }
            Some(ValueField::Integers(_)) => Err(format!("X 二进制 {label} 类型错误：预期浮点数")),
            Some(ValueField::String(_)) => Err(format!("X 二进制 {label} 类型错误：预期浮点数")),
            None => Err(format!("X 二进制 {label} 数据不完整")),
        }
    }

    fn next_float_optional(&mut self) -> Result<Option<f32>, String> {
        self.advance_empty();
        match self.fields.get(self.field_index) {
            Some(ValueField::Floats(values)) => {
                let Some(value) = values.get(self.value_index).copied() else {
                    return Ok(None);
                };
                self.value_index += 1;
                Ok(Some(value))
            }
            Some(ValueField::Integers(_)) => Err("X 二进制材质数据类型错误".to_owned()),
            Some(ValueField::String(_)) => Err("X 二进制材质数据类型错误".to_owned()),
            None => Ok(None),
        }
    }

    fn advance_empty(&mut self) {
        while let Some(field) = self.fields.get(self.field_index) {
            let length = match field {
                ValueField::Integers(values) => values.len(),
                ValueField::Floats(values) => values.len(),
                ValueField::String(_) => 1,
            };
            if self.value_index < length {
                break;
            }
            self.field_index += 1;
            self.value_index = 0;
        }
    }
}

pub(crate) fn is_binary_x(data: &[u8]) -> bool {
    data.starts_with(b"xof ") && data.get(8..11) == Some(b"bin")
}

pub(crate) fn parse_binary_x(data: &[u8]) -> Result<AccessoryParsedManifest, String> {
    if !is_binary_x(data) || data.len() < 16 {
        return Err("无效的二进制 X 文件头".to_owned());
    }
    let float_width = match &data[12..16] {
        b"0032" => 4,
        b"0064" => 8,
        _ => return Err("X 二进制文件使用了不支持的浮点精度".to_owned()),
    };
    let header = std::str::from_utf8(&data[..16])
        .map_err(|_| "X 二进制文件头不是 ASCII".to_owned())?
        .to_owned();
    let roots = Reader::new(data, float_width).parse_document()?;
    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    for root in &roots {
        collect_meshes(root, data.len(), &mut meshes, &mut materials)?;
    }
    let mut texture_references = Vec::new();
    let mut unique_texture_references = HashSet::new();
    for material in &materials {
        for reference in &material.texture_references {
            if unique_texture_references.insert(reference.clone()) {
                texture_references.push(reference.clone());
            }
        }
    }
    let mut diagnostics = Vec::new();
    if meshes.is_empty() {
        diagnostics.push(AccessoryDiagnostic {
            level: "warning".to_owned(),
            code: "X_BINARY_NO_MESH".to_owned(),
            message: "Binary DirectX .x file contains no supported Mesh objects.".to_owned(),
        });
    }
    Ok(AccessoryParsedManifest {
        format: "x".to_owned(),
        byte_length: data.len(),
        text: false,
        header,
        mesh_count: meshes.len(),
        material_count: materials.len(),
        mesh_summaries: meshes,
        materials,
        vac_settings: None,
        texture_references,
        diagnostics,
    })
}

fn collect_meshes(
    node: &Node,
    source_length: usize,
    meshes: &mut Vec<AccessoryMeshSummary>,
    materials: &mut Vec<AccessoryMaterial>,
) -> Result<(), String> {
    if node.kind == "Mesh" {
        let (mut mesh, mut mesh_materials) = parse_mesh(node, source_length)?;
        mesh.material_start_index = materials.len();
        if mesh_materials.len() < mesh.material_count {
            let missing = mesh.material_count - mesh_materials.len();
            mesh_materials
                .try_reserve(missing)
                .map_err(|_| "X 二进制材质列表内存不足".to_owned())?;
            mesh_materials.extend((0..missing).map(|_| empty_material()));
        }
        meshes
            .try_reserve(1)
            .map_err(|_| "X 二进制网格列表内存不足".to_owned())?;
        materials
            .try_reserve(mesh_materials.len())
            .map_err(|_| "X 二进制材质列表内存不足".to_owned())?;
        materials.extend(mesh_materials);
        meshes.push(mesh);
        return Ok(());
    }
    for child in &node.children {
        collect_meshes(child, source_length, meshes, materials)?;
    }
    Ok(())
}

fn parse_mesh(
    node: &Node,
    source_length: usize,
) -> Result<(AccessoryMeshSummary, Vec<AccessoryMaterial>), String> {
    let mut values = ValueCursor::new(&node.fields);
    let vertex_count = checked_count(
        values.next_integer("Mesh 顶点数量")?,
        source_length / 12,
        "Mesh 顶点数量",
    )?;
    let mut positions = Vec::new();
    positions
        .try_reserve_exact(vertex_count)
        .map_err(|_| "X 二进制 Mesh 顶点内存不足".to_owned())?;
    for _ in 0..vertex_count {
        positions.push([
            values.next_float("Mesh 顶点 X")?,
            values.next_float("Mesh 顶点 Y")?,
            values.next_float("Mesh 顶点 Z")?,
        ]);
    }
    let face_count = checked_count(
        values.next_integer("Mesh 面数量")?,
        source_length / 4,
        "Mesh 面数量",
    )?;
    let mut face_indices = Vec::new();
    face_indices
        .try_reserve_exact(face_count)
        .map_err(|_| "X 二进制 Mesh 面列表内存不足".to_owned())?;
    let mut total_indices = 0usize;
    for _ in 0..face_count {
        let count = checked_count(
            values.next_integer("Mesh 面顶点数量")?,
            source_length / 4,
            "Mesh 面顶点数量",
        )?;
        total_indices = total_indices
            .checked_add(count)
            .filter(|total| *total <= source_length / 4)
            .ok_or_else(|| "X 二进制 Mesh 索引数量超出文件长度".to_owned())?;
        let mut face = Vec::new();
        face.try_reserve_exact(count)
            .map_err(|_| "X 二进制 Mesh 面索引内存不足".to_owned())?;
        for _ in 0..count {
            let index = values.next_integer("Mesh 面顶点索引")?;
            if usize::try_from(index).map_or(true, |index| index >= vertex_count) {
                return Err("X 二进制 Mesh 面引用了范围外的顶点".to_owned());
            }
            face.push(index);
        }
        face_indices.push(face);
    }

    let mut summary = AccessoryMeshSummary {
        vertex_count,
        face_count,
        positions,
        face_indices,
        normals: Vec::new(),
        normal_face_indices: Vec::new(),
        texture_coordinates: Vec::new(),
        vertex_colors: Vec::new(),
        material_indices: Vec::new(),
        material_start_index: 0,
        material_count: 0,
    };
    let mut mesh_materials = Vec::new();
    for child in &node.children {
        match child.kind.as_str() {
            "MeshNormals" => {
                let (normals, face_indices) = parse_normals(child, source_length)?;
                summary.normals = normals;
                summary.normal_face_indices = face_indices;
            }
            "MeshTextureCoords" => {
                summary.texture_coordinates = parse_texture_coordinates(child, source_length)?;
            }
            "MeshVertexColors" => {
                summary.vertex_colors = parse_vertex_colors(child, source_length)?;
            }
            "MeshMaterialList" => {
                let (material_count, material_indices, parsed_materials) =
                    parse_material_list(child, source_length)?;
                summary.material_count = material_count;
                summary.material_indices = material_indices;
                mesh_materials = parsed_materials;
            }
            _ => {}
        }
    }
    Ok((summary, mesh_materials))
}

fn parse_normals(
    node: &Node,
    source_length: usize,
) -> Result<(Vec<[f32; 3]>, Vec<Vec<u32>>), String> {
    let mut values = ValueCursor::new(&node.fields);
    let count = checked_count(
        values.next_integer("MeshNormals 法线数量")?,
        source_length / 12,
        "MeshNormals 法线数量",
    )?;
    let mut normals = Vec::new();
    normals
        .try_reserve_exact(count)
        .map_err(|_| "X 二进制法线列表内存不足".to_owned())?;
    for _ in 0..count {
        normals.push([
            values.next_float("MeshNormals 法线 X")?,
            values.next_float("MeshNormals 法线 Y")?,
            values.next_float("MeshNormals 法线 Z")?,
        ]);
    }
    let face_count = checked_count(
        values.next_integer("MeshNormals 面数量")?,
        source_length / 4,
        "MeshNormals 面数量",
    )?;
    let mut face_indices = Vec::new();
    face_indices
        .try_reserve_exact(face_count)
        .map_err(|_| "X 二进制法线面列表内存不足".to_owned())?;
    for _ in 0..face_count {
        let count = checked_count(
            values.next_integer("MeshNormals 面顶点数量")?,
            source_length / 4,
            "MeshNormals 面顶点数量",
        )?;
        let mut face = Vec::new();
        face.try_reserve_exact(count)
            .map_err(|_| "X 二进制法线索引内存不足".to_owned())?;
        for _ in 0..count {
            let index = values.next_integer("MeshNormals 法线索引")?;
            if usize::try_from(index).map_or(true, |index| index >= normals.len()) {
                return Err("X 二进制 MeshNormals 引用了范围外的法线".to_owned());
            }
            face.push(index);
        }
        face_indices.push(face);
    }
    Ok((normals, face_indices))
}

fn parse_texture_coordinates(node: &Node, source_length: usize) -> Result<Vec<[f32; 2]>, String> {
    let mut values = ValueCursor::new(&node.fields);
    let count = checked_count(
        values.next_integer("MeshTextureCoords 数量")?,
        source_length / 8,
        "MeshTextureCoords 数量",
    )?;
    let mut coordinates = Vec::new();
    coordinates
        .try_reserve_exact(count)
        .map_err(|_| "X 二进制 UV 列表内存不足".to_owned())?;
    for _ in 0..count {
        coordinates.push([
            values.next_float("MeshTextureCoords U")?,
            values.next_float("MeshTextureCoords V")?,
        ]);
    }
    Ok(coordinates)
}

fn parse_vertex_colors(
    node: &Node,
    source_length: usize,
) -> Result<Vec<AccessoryVertexColor>, String> {
    let mut values = ValueCursor::new(&node.fields);
    let count = checked_count(
        values.next_integer("MeshVertexColors 数量")?,
        source_length / 20,
        "MeshVertexColors 数量",
    )?;
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(count)
        .map_err(|_| "X 二进制顶点颜色列表内存不足".to_owned())?;
    for _ in 0..count {
        let vertex_index = values.next_integer("MeshVertexColors 顶点索引")?;
        let color = [
            values.next_float("MeshVertexColors 红色")?,
            values.next_float("MeshVertexColors 绿色")?,
            values.next_float("MeshVertexColors 蓝色")?,
            values.next_float("MeshVertexColors 透明度")?,
        ];
        colors.push(AccessoryVertexColor {
            vertex_index,
            color,
        });
    }
    Ok(colors)
}

fn parse_material_list(
    node: &Node,
    source_length: usize,
) -> Result<(usize, Vec<u32>, Vec<AccessoryMaterial>), String> {
    let mut values = ValueCursor::new(&node.fields);
    let material_count = checked_count(
        values.next_integer("MeshMaterialList 材质数量")?,
        source_length / 4,
        "MeshMaterialList 材质数量",
    )?;
    let face_count = checked_count(
        values.next_integer("MeshMaterialList 面材质索引数量")?,
        source_length / 4,
        "MeshMaterialList 面材质索引数量",
    )?;
    let mut indices = Vec::new();
    indices
        .try_reserve_exact(face_count)
        .map_err(|_| "X 二进制面材质索引内存不足".to_owned())?;
    for _ in 0..face_count {
        let index = values.next_integer("MeshMaterialList 面材质索引")?;
        if usize::try_from(index).map_or(true, |index| index >= material_count) {
            return Err("X 二进制 MeshMaterialList 引用了范围外的材质".to_owned());
        }
        indices.push(index);
    }
    let mut materials = Vec::new();
    materials
        .try_reserve_exact(material_count.min(node.children.len()))
        .map_err(|_| "X 二进制材质列表内存不足".to_owned())?;
    for child in &node.children {
        if child.kind == "Material" {
            materials.push(parse_material(child)?);
        }
    }
    Ok((material_count, indices, materials))
}

fn parse_material(node: &Node) -> Result<AccessoryMaterial, String> {
    let mut values = ValueCursor::new(&node.fields);
    let first = values.next_float_optional()?;
    let (face_color, power, specular_color, emissive_color) = if let Some(first) = first {
        let mut diffuse = [0.0; 4];
        diffuse[0] = first;
        for value in diffuse.iter_mut().skip(1) {
            *value = values.next_float("Material 漫反射颜色")?;
        }
        let power = values.next_float("Material 高光强度")?;
        let mut specular = [0.0; 3];
        for value in &mut specular {
            *value = values.next_float("Material 高光颜色")?;
        }
        let mut emissive = [0.0; 3];
        for value in &mut emissive {
            *value = values.next_float("Material 自发光颜色")?;
        }
        (Some(diffuse), Some(power), Some(specular), Some(emissive))
    } else {
        (None, None, None, None)
    };
    let mut texture_references = Vec::new();
    for child in &node.children {
        if child.kind == "TextureFilename" {
            texture_references.extend(child.fields.iter().filter_map(|field| match field {
                ValueField::String(value) if !value.trim().is_empty() => Some(value.clone()),
                _ => None,
            }));
        }
    }
    Ok(AccessoryMaterial {
        name: node.name.clone(),
        face_color,
        power,
        specular_color,
        emissive_color,
        texture_references,
    })
}

fn empty_material() -> AccessoryMaterial {
    AccessoryMaterial {
        name: None,
        face_color: None,
        power: None,
        specular_color: None,
        emissive_color: None,
        texture_references: Vec::new(),
    }
}

fn checked_count(value: u32, maximum: usize, label: &str) -> Result<usize, String> {
    let count = usize::try_from(value).map_err(|_| format!("X 二进制 {label} 超出平台范围"))?;
    if count > maximum {
        return Err(format!("X 二进制 {label} 超出文件长度"));
    }
    Ok(count)
}
