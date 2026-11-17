// 1. The GUI exe gets an asInvoker manifest: a plain click must not prompt.
//    It gets admin rights itself (src/elevation.rs).
// 2. Both exes get the app icon (drawn by src/icon.rs) as a Windows resource.

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=assets/icon-256.rgba");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg-bin=dragonslayer=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bin=dragonslayer=/MANIFESTUAC:level='asInvoker'");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.ico");
    std::fs::write(&out, icon::ico(&[16, 20, 24, 32, 40, 48, 64, 256])).expect("write icon");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().unwrap());
    res.set("ProductName", "DragonSlayer");
    res.set("FileDescription", "DragonSlayer");
    res.set("CompanyName", "");
    res.compile().expect("compile Windows resources (needs rc.exe from the Windows SDK)");
}
