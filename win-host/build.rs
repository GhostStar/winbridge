use std::{env, fs, path::PathBuf};

fn append_res_entry(out: &mut Vec<u8>, res_type: u16, res_id: u16, data: &[u8]) {
    let data_size = data.len() as u32;
    let header_size: u32 = 32;
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&header_size.to_le_bytes());
    // Type: ordinal (0xFFFF, res_type)
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_type.to_le_bytes());
    // Name: ordinal (0xFFFF, res_id)
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_id.to_le_bytes());
    // DataVersion (4), MemoryFlags (2: MOVEABLE|PURE|DISCARDABLE = 0x1030), LanguageId (2: en-US 0x0409)
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0x1030u16.to_le_bytes());
    out.extend_from_slice(&0x0409u16.to_le_bytes());
    // Version (4), Characteristics (4)
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn build_windows_icon_res(ico_bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if ico_bytes.len() < 6 {
        return Err("winbridge.ico too short".into());
    }
    let count = u16::from_le_bytes([ico_bytes[4], ico_bytes[5]]) as usize;
    if count == 0 || ico_bytes.len() < 6 + count * 16 {
        return Err("invalid ICO directory header".into());
    }

    let mut res = Vec::with_capacity(ico_bytes.len() + 256);
    // Required 32-byte null header for 32-bit Win32 .res files
    append_res_entry(&mut res, 0, 0, &[]);

    let mut grp = Vec::with_capacity(6 + count * 14);
    grp.extend_from_slice(&0u16.to_le_bytes());
    grp.extend_from_slice(&1u16.to_le_bytes());
    grp.extend_from_slice(&(count as u16).to_le_bytes());

    for i in 0..count {
        let off = 6 + i * 16;
        let entry = &ico_bytes[off..off + 16];
        let img_size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let img_off = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if img_off + img_size > ico_bytes.len() {
            return Err("ICO image entry out of bounds".into());
        }
        let icon_id = (i + 1) as u16;
        // RT_ICON = 3
        append_res_entry(&mut res, 3, icon_id, &ico_bytes[img_off..img_off + img_size]);
        // GRPICONDIRENTRY: first 12 bytes of ICONDIRENTRY + 2-byte nId
        grp.extend_from_slice(&entry[..12]);
        grp.extend_from_slice(&icon_id.to_le_bytes());
    }
    // RT_GROUP_ICON = 14, ID = 1
    append_res_entry(&mut res, 14, 1, &grp);
    Ok(res)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    println!("cargo:rerun-if-changed=../protocol/control.proto");
    prost_build::compile_protos(&["../protocol/control.proto"], &["../protocol"])?;

    let ico_path = PathBuf::from("assets/winbridge.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && ico_path.exists()
    {
        let ico_bytes = fs::read(&ico_path)?;
        let res_bytes = build_windows_icon_res(&ico_bytes)?;
        let out_dir = PathBuf::from(env::var("OUT_DIR")?);
        let res_path = out_dir.join("winbridge.res");
        fs::write(&res_path, res_bytes)?;
        println!("cargo:rustc-link-arg-bin=win-host={}", res_path.display());
    }
    Ok(())
}
