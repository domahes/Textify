//! Regenerates `assets/TextifyRS.ico` from the shared application artwork.
//!
//! Usage: `cargo run --bin gen_icon [output.ico]`
//!
//! The build script embeds the resulting file into the executable, so rerun
//! this after changing `src/icon_art.rs` and then rebuild.

use std::env;
use std::fs;
use std::io;
use std::path::Path;

const SIZES: [u32; 5] = [16, 24, 32, 48, 256];

fn main() -> io::Result<()> {
    let output = env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/TextifyRS.ico".to_string());

    let images: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&size| bmp_icon_image(size, &textify_rs::icon_art::rasterize(size as usize)))
        .collect();

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    for (&size, image) in SIZES.iter().zip(&images) {
        let stored = if size == 256 { 0 } else { size as u8 };
        ico.push(stored); // width
        ico.push(stored); // height
        ico.push(0); // palette colors
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // color planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in &images {
        ico.extend_from_slice(image);
    }

    if let Some(parent) = Path::new(&output).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, &ico)?;
    println!("{output}: {} bytes, sizes {SIZES:?}", ico.len());
    Ok(())
}

/// Packs top-down straight-alpha pixels into the DIB payload of an ICO entry:
/// BITMAPINFOHEADER with doubled height, bottom-up BGRA rows, zero AND mask.
fn bmp_icon_image(size: u32, pixels: &[u32]) -> Vec<u8> {
    let mask_stride = (size + 31) / 32 * 4;
    let xor_size = size * size * 4;
    let mask_size = mask_stride * size;

    let mut data = Vec::with_capacity(40 + (xor_size + mask_size) as usize);
    data.extend_from_slice(&40u32.to_le_bytes()); // biSize
    data.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    data.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight: XOR + AND
    data.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    data.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    data.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    data.extend_from_slice(&(xor_size + mask_size).to_le_bytes()); // biSizeImage
    data.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    data.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    data.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    data.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..size).rev() {
        for x in 0..size {
            let pixel = pixels[(y * size + x) as usize];
            data.extend_from_slice(&pixel.to_le_bytes()); // BGRA
        }
    }

    // The 1bpp transparency mask is all zero: the alpha channel does the work.
    data.resize(data.len() + mask_size as usize, 0);
    data
}
