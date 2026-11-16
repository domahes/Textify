//! Builds a single COFF resource object holding the application manifest and
//! every icon image. One object means one `.rsrc` tree, so there is no need to
//! merge resource sections coming from different objects.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Write};

use super::coff::{
    CoffWriter, MachineType, resource_data_entry, resource_directory_id_entry,
    resource_directory_table,
};

pub const RT_ICON: u32 = 3;
pub const RT_GROUP_ICON: u32 = 14;
pub const RT_MANIFEST: u32 = 24;
pub const LANG_EN_US: u32 = 1033;

/// Resources grouped as type -> name -> (language, payload), ordered so the
/// directory entries come out sorted by ID as Windows expects.
#[derive(Default)]
pub struct ResourceSet {
    types: BTreeMap<u32, BTreeMap<u32, (u32, Vec<u8>)>>,
}

impl ResourceSet {
    pub fn add(&mut self, type_id: u32, name_id: u32, lang: u32, data: Vec<u8>) {
        self.types
            .entry(type_id)
            .or_default()
            .insert(name_id, (lang, data));
    }

    pub fn build(&self, arch: MachineType) -> io::Result<Vec<u8>> {
        let type_count = self.types.len() as u32;
        let resource_count = self.types.values().map(|names| names.len()).sum::<usize>();

        // Pass 1: lay out directory tables, data entries, then aligned blobs.
        let mut offset = 16 + 8 * type_count;
        let mut name_table_offsets = Vec::new();
        for names in self.types.values() {
            name_table_offsets.push(offset);
            offset += 16 + 8 * names.len() as u32;
        }
        let mut lang_table_offsets = Vec::new();
        for _ in 0..resource_count {
            lang_table_offsets.push(offset);
            offset += 24; // one table plus one entry
        }
        let mut data_entry_offsets = Vec::new();
        for _ in 0..resource_count {
            data_entry_offsets.push(offset);
            offset += 16;
        }
        let mut blob_offsets = Vec::new();
        for names in self.types.values() {
            for (_, data) in names.values() {
                offset = offset.div_ceil(8) * 8;
                blob_offsets.push(offset);
                offset += data.len() as u32;
            }
        }

        // Pass 2: write the same layout, recording data entry relocations.
        let mut obj = CoffWriter::new(Cursor::new(Vec::new()), arch)?;

        obj.add_data(&resource_directory_table(type_count as u16))?;
        for (index, type_id) in self.types.keys().enumerate() {
            obj.add_data(&resource_directory_id_entry(
                *type_id,
                name_table_offsets[index],
                true,
            ))?;
        }

        let mut lang_index = 0;
        for names in self.types.values() {
            obj.add_data(&resource_directory_table(names.len() as u16))?;
            for name_id in names.keys() {
                obj.add_data(&resource_directory_id_entry(
                    *name_id,
                    lang_table_offsets[lang_index],
                    true,
                ))?;
                lang_index += 1;
            }
        }

        let mut data_index = 0;
        for names in self.types.values() {
            for (lang, _) in names.values() {
                obj.add_data(&resource_directory_table(1))?;
                obj.add_data(&resource_directory_id_entry(
                    *lang,
                    data_entry_offsets[data_index],
                    false,
                ))?;
                data_index += 1;
            }
        }

        let mut relocations = Vec::new();
        let mut blob_index = 0;
        for names in self.types.values() {
            for (_, data) in names.values() {
                let address = obj.add_data(&resource_data_entry(
                    blob_offsets[blob_index],
                    data.len() as u32,
                ))?;
                relocations.push(address);
                blob_index += 1;
            }
        }

        for names in self.types.values() {
            for (_, data) in names.values() {
                obj.align_to(8)?;
                obj.add_data(data)?;
            }
        }

        for address in relocations {
            obj.add_relocation(address)?;
        }

        Ok(obj.finish()?.into_inner())
    }
}

/// One image extracted from an `.ico` file.
pub struct IconImage {
    pub width: u8,
    pub height: u8,
    pub data: Vec<u8>,
}

/// Splits an `.ico` into its image payloads, ready to become RT_ICON entries.
pub fn parse_ico(bytes: &[u8]) -> io::Result<Vec<IconImage>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed ICO file");
    if bytes.len() < 6 || u16::from_le_bytes([bytes[0], bytes[1]]) != 0 {
        return Err(invalid());
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut images = Vec::with_capacity(count);

    for index in 0..count {
        let entry = 6 + index * 16;
        if entry + 16 > bytes.len() {
            return Err(invalid());
        }
        let size = u32::from_le_bytes(bytes[entry + 8..entry + 12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(bytes[entry + 12..entry + 16].try_into().unwrap()) as usize;
        if offset + size > bytes.len() {
            return Err(invalid());
        }
        images.push(IconImage {
            width: bytes[entry],
            height: bytes[entry + 1],
            data: bytes[offset..offset + size].to_vec(),
        });
    }
    Ok(images)
}

/// Builds the RT_GROUP_ICON payload referencing the given RT_ICON IDs.
pub fn group_icon_dir(images: &[IconImage], first_id: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + 14 * images.len());
    out.write_all(&0u16.to_le_bytes()).unwrap(); // reserved
    out.write_all(&1u16.to_le_bytes()).unwrap(); // type: icon
    out.write_all(&(images.len() as u16).to_le_bytes()).unwrap();
    for (index, image) in images.iter().enumerate() {
        out.write_all(&[image.width, image.height, 0, 0]).unwrap();
        out.write_all(&1u16.to_le_bytes()).unwrap(); // planes
        out.write_all(&32u16.to_le_bytes()).unwrap(); // bits per pixel
        out.write_all(&(image.data.len() as u32).to_le_bytes()).unwrap();
        out.write_all(&(first_id + index as u16).to_le_bytes()).unwrap();
    }
    out
}
