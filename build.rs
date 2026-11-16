//! Build script: embeds the application manifest and the icon into the exe.
//!
//! On GNU targets both go into a single COFF object with a `.rsrc` section
//! (see `build/coff.rs` and `build/resources.rs`), so no external resource
//! compiler is needed. Regenerate the icon with `cargo run --bin gen_icon`
//! after editing `src/icon_art.rs`.

#[path = "build/coff.rs"]
mod coff;
#[path = "build/resources.rs"]
mod resources;

use std::env;
use std::fs;
use std::path::Path;

use coff::MachineType;
use embed_manifest::manifest::{DpiAwareness, ManifestBuilder};
use resources::{LANG_EN_US, RT_GROUP_ICON, RT_ICON, RT_MANIFEST, ResourceSet};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/coff.rs");
    println!("cargo:rerun-if-changed=build/resources.rs");
    println!("cargo:rerun-if-changed=assets/TextifyRS.ico");

    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let target = env::var("TARGET").expect("TARGET is set for build scripts");
    if target.ends_with("windows-msvc") {
        // link.exe does not accept COFF resource objects; use the SDK manifest
        // mechanism there. Icon embedding is currently implemented for GNU.
        embed_manifest::embed_manifest(manifest_builder())
            .expect("unable to embed the application manifest");
        return;
    }

    let arch = match target.split('-').next() {
        Some("i686") => MachineType::I386,
        Some("x86_64") => MachineType::X86_64,
        Some("aarch64") => MachineType::Aarch64,
        _ => panic!("unsupported target architecture: {target}"),
    };

    // Optional so the first build can bootstrap: gen_icon itself needs the
    // build script to succeed before it can create the icon file.
    let images = match fs::read("assets/TextifyRS.ico") {
        Ok(ico) => resources::parse_ico(&ico).expect("assets/TextifyRS.ico is malformed"),
        Err(_) => {
            println!(
                "cargo:warning=assets/TextifyRS.ico missing; building without the exe icon. Run `cargo run --bin gen_icon` and rebuild."
            );
            Vec::new()
        }
    };

    let mut set = ResourceSet::default();
    set.add(
        RT_MANIFEST,
        1,
        LANG_EN_US,
        manifest_builder().to_string().into_bytes(),
    );
    for (index, image) in images.iter().enumerate() {
        set.add(RT_ICON, index as u32 + 1, LANG_EN_US, image.data.clone());
    }
    if !images.is_empty() {
        set.add(
            RT_GROUP_ICON,
            1,
            LANG_EN_US,
            resources::group_icon_dir(&images, 1),
        );
    }

    let object = set.build(arch).expect("unable to build the resource object");
    let object_path = Path::new(&env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("textify-resources.o");
    fs::write(&object_path, object).expect("unable to write the resource object");
    println!("cargo:rustc-link-arg-bins={}", object_path.display());
}

fn manifest_builder() -> ManifestBuilder {
    embed_manifest::new_manifest("TextifyRS").dpi_awareness(DpiAwareness::PerMonitorV2)
}
