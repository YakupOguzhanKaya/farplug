fn main() {
    println!("cargo::rerun-if-changed=../../dist/icon/farplug.ico");
    // The exe's icon, which Explorer and the Start menu shortcut show. Release
    // builds use MSVC, whose resource compiler the Windows runners have.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../dist/icon/farplug.ico");
        res.compile().expect("embedding the Windows icon");
    }
}
