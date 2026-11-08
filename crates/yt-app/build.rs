fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon_with_id("../../packaging/icons/icon.ico", "32512")
            .set("ProductName", "YT Ultimate")
            .set("FileDescription", "YT Ultimate")
            .set("OriginalFilename", "yt-ultimate.exe")
            .compile()
            .expect("Windows executable resources require the Windows SDK resource compiler");
    }
}
