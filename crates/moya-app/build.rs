fn main() {
    println!("cargo:rerun-if-changed=assets/Moya.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/Moya.ico")
            .set("FileDescription", "Moya GPU Glow")
            .set("ProductName", "Moya")
            .set("OriginalFilename", "Moya.exe")
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            .set("ProductVersion", env!("CARGO_PKG_VERSION"))
            .compile().expect("compiling Moya Windows icon/version resources");
    }
}
