#![allow(dead_code)]

pub fn floats(out: &mut Vec<u8>, values: &[f32]) { for value in values { out.extend(value.to_le_bytes()); } }
fn text(out: &mut Vec<u8>, value: &str, utf16: bool) {
    let bytes = if utf16 { value.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>() } else { value.as_bytes().to_vec() };
    out.extend((bytes.len() as i32).to_le_bytes()); out.extend(bytes);
}
fn index(out: &mut Vec<u8>, value: i32, width: u8) { out.extend(&value.to_le_bytes()[..width as usize]); }

pub fn pmx(utf16: bool, width: u8, mode: u8, additional_uv: bool) -> Vec<u8> {
    pmx_mesh(utf16,width,mode,additional_uv,
        &[([-1.0,0.0,0.0],[0.0,0.0,-1.0],[0.0,0.0]),([1.0,0.0,0.0],[0.0,0.0,-1.0],[1.0,0.0]),([0.0,2.0,0.0],[0.0,0.0,-1.0],[0.5,1.0])],
        &[0,1,2])
}

pub fn pmx_mesh(utf16: bool, width: u8, mode: u8, additional_uv: bool,
    vertices: &[([f32;3],[f32;3],[f32;2])], indices: &[u32]) -> Vec<u8> {
    let mut out = b"PMX ".to_vec(); floats(&mut out, &[2.1]);
    out.extend([8, u8::from(!utf16), u8::from(additional_uv), width, width, width, width, width, width]);
    for value in ["テスト模型", "Fixture", "", ""] { text(&mut out, value, utf16); }
    out.extend((vertices.len() as i32).to_le_bytes());
    for &(position, normal, uv) in vertices {
        floats(&mut out, &position); floats(&mut out, &normal); floats(&mut out, &uv);
        if additional_uv { floats(&mut out, &[1.0-uv[0], 1.0-uv[1], 0.0, 1.0]); }
        out.push(mode);
        match mode {
            0 => index(&mut out, 0, width),
            1 | 3 => { index(&mut out, 0, width); index(&mut out, 1, width); floats(&mut out, &[0.5]);
                if mode == 3 { floats(&mut out, &[position[0],position[1],position[2],position[0]-0.5,position[1],position[2],position[0]+0.5,position[1],position[2]]); } },
            2 | 4 => { for bone in [0,1,0,0] { index(&mut out, bone, width); } floats(&mut out, &[0.5,0.5,0.0,0.0]); },
            _ => panic!("invalid fixture mode"),
        }
        floats(&mut out, &[1.0]);
    }
    out.extend((indices.len() as i32).to_le_bytes()); for vertex in indices { index(&mut out, *vertex as i32, width); }
    out.extend(1i32.to_le_bytes()); text(&mut out, "貼図\\tex.png", utf16);
    out.extend(1i32.to_le_bytes()); text(&mut out, "材質", utf16); text(&mut out, "", utf16);
    floats(&mut out, &[0.65,0.8,0.95,1.0, 0.1,0.1,0.1,12.0, 0.05,0.05,0.05]); out.push(1);
    floats(&mut out, &[0.0,0.0,0.0,1.0,1.0]); index(&mut out, 0, width); index(&mut out, -1, width);
    out.extend([0,1,0]); text(&mut out, "", utf16); out.extend((indices.len() as i32).to_le_bytes());
    out.extend(2i32.to_le_bytes());
    for bone in 0..2 { text(&mut out, if bone == 0 { "センター" } else { "頭" }, utf16); text(&mut out, "", utf16);
        floats(&mut out, &[0.0,bone as f32,0.0]); index(&mut out, if bone == 0 { -1 } else { 0 }, width);
        out.extend(0i32.to_le_bytes()); out.extend(0u16.to_le_bytes()); floats(&mut out, &[0.0,0.5,0.0]); }
    for _ in 0..5 { out.extend(0i32.to_le_bytes()); }
    out
}

pub fn pmd(extended: bool, texture: &str) -> Vec<u8> {
    let mut out = b"Pmd".to_vec(); floats(&mut out, &[1.0]);
    let mut name = [0u8; 20]; name[..7].copy_from_slice(b"Fixture"); out.extend(name); out.extend([0;256]);
    out.extend(3u32.to_le_bytes());
    for (position, uv) in [([-1.0,0.0,0.0], [0.0,0.0]), ([1.0,0.0,0.0], [1.0,0.0]), ([0.0,2.0,0.0], [0.5,1.0])] {
        floats(&mut out, &position); floats(&mut out, &[0.0,0.0,-1.0]); floats(&mut out, &uv);
        out.extend(0u16.to_le_bytes()); out.extend(1u16.to_le_bytes()); out.extend([50,1]);
    }
    out.extend(3u32.to_le_bytes()); for vertex in [0u16,1,2] { out.extend(vertex.to_le_bytes()); }
    out.extend(1u32.to_le_bytes()); floats(&mut out, &[0.65,0.8,0.95,1.0,12.0, 0.1,0.1,0.1, 0.05,0.05,0.05]);
    out.extend([255,0]); out.extend(3u32.to_le_bytes());
    let mut texture_name = [0u8;20]; texture_name[..texture.len()].copy_from_slice(texture.as_bytes()); out.extend(texture_name);
    out.extend(2u16.to_le_bytes());
    for bone in 0..2 { let mut name = [0u8;20]; name[..4].copy_from_slice(if bone == 0 { b"root" } else { b"head" }); out.extend(name);
        out.extend((if bone == 0 { u16::MAX } else { 0u16 }).to_le_bytes()); out.extend(u16::MAX.to_le_bytes()); out.push(0);
        out.extend(u16::MAX.to_le_bytes()); floats(&mut out, &[0.0,bone as f32,0.0]); }
    out.extend(0u16.to_le_bytes()); out.extend(0u16.to_le_bytes());
    if extended { out.extend([0,0]); out.extend(0u32.to_le_bytes()); out.push(1);
        out.extend([0;276]); out.extend([0;40]); out.extend([0;1000]); out.extend(0u32.to_le_bytes()); out.extend(0u32.to_le_bytes()); }
    out
}
