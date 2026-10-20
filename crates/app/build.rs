// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the application icon (`assets/icon/opensmarters.ico`) into the Windows
//! executable so Explorer and the taskbar show it before the window opens.

fn main() {
    println!("cargo:rerun-if-changed=opensmarters.rc");
    println!("cargo:rerun-if-changed=../../assets/icon/opensmarters.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // A missing resource compiler only costs the exe icon (the window icon is set at
        // runtime), so warn instead of failing the build.
        if let Err(e) =
            embed_resource::compile("opensmarters.rc", embed_resource::NONE).manifest_optional()
        {
            println!("cargo:warning=could not embed the exe icon: {e}");
        }
    }
}
