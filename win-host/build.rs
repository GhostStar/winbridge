fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    println!("cargo:rerun-if-changed=../protocol/control.proto");
    prost_build::compile_protos(&["../protocol/control.proto"], &["../protocol"])?;
    Ok(())
}
