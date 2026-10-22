fn main() {
    // 非 Windows 目标时 embed-resource 什么也不做
    println!("cargo:rerun-if-changed=scell.rc");
    println!("cargo:rerun-if-changed=../../assets/icons/scell.ico");
    embed_resource::compile("scell.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("无法嵌入 Windows 资源");
}
