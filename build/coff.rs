//! Just enough COFF support to write a `.rsrc` section for GNU Windows
//! targets, so resources link without rc.exe or windres.
//!
//! Adapted from the `embed-manifest` crate (MIT,
//! <https://codeberg.org/carey/embed-manifest>), extended to hold any number
//! of resources instead of a single manifest.

use std::io::{self, Seek, SeekFrom, Write};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy)]
pub enum MachineType {
    I386,
    X86_64,
    Aarch64,
}

impl MachineType {
    pub fn machine(&self) -> u16 {
        match self {
            Self::I386 => 0x014c,
            Self::X86_64 => 0x8664,
            Self::Aarch64 => 0xaa64,
        }
    }

    pub fn relocation_type(&self) -> u16 {
        match self {
            Self::I386 => 7,     // ADDR32NB
            Self::X86_64 => 3,   // ADDR32NB
            Self::Aarch64 => 2,  // ADDR32NB
        }
    }
}

pub struct CoffWriter<W> {
    writer: W,
    machine_type: MachineType,
    size_of_raw_data: u32,
    number_of_relocations: u16,
}

impl<W: Write + Seek> CoffWriter<W> {
    pub fn new(mut writer: W, machine_type: MachineType) -> io::Result<Self> {
        // Reserve space for the file header and one section header.
        writer.write_all(&[0; 60])?;
        Ok(Self {
            writer,
            machine_type,
            size_of_raw_data: 0,
            number_of_relocations: 0,
        })
    }

    /// Appends data to the section and returns its offset within the section.
    pub fn add_data(&mut self, data: &[u8]) -> io::Result<u32> {
        let start = self.size_of_raw_data;
        self.writer.write_all(data)?;
        self.size_of_raw_data = start + data.len() as u32;
        Ok(start)
    }

    pub fn align_to(&mut self, n: u32) -> io::Result<()> {
        let offset = self.size_of_raw_data % n;
        if offset != 0 {
            let padding = n - offset;
            for _ in 0..padding {
                self.writer.write_all(&[0])?;
            }
            self.size_of_raw_data += padding;
        }
        Ok(())
    }

    /// Writes an ADDR32NB relocation against the `.rsrc` section symbol.
    pub fn add_relocation(&mut self, address: u32) -> io::Result<()> {
        self.number_of_relocations += 1;
        self.writer.write_all(&address.to_le_bytes())?;
        self.writer.write_all(&[0, 0, 0, 0])?; // symbol table index 0: .rsrc
        self.writer.write_all(&self.machine_type.relocation_type().to_le_bytes())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);

        let pointer_to_symbol_table = self.writer.stream_position()? as u32;

        // Section symbol plus its auxiliary record.
        self.writer.write_all(b".rsrc\0\0\0")?;
        self.writer.write_all(&[0, 0, 0, 0])?; // value
        self.writer.write_all(&[1, 0])?; // section number, 1-based
        self.writer.write_all(&[0, 0, 3, 1])?; // type 0, static, 1 aux symbol
        self.writer.write_all(&self.size_of_raw_data.to_le_bytes())?;
        self.writer.write_all(&self.number_of_relocations.to_le_bytes())?;
        self.writer.write_all(&[0; 12])?;

        // Empty string table.
        self.writer.write_all(&[0; 4])?;

        let end_of_file = self.writer.seek(SeekFrom::Start(0))?;
        self.writer.write_all(&self.machine_type.machine().to_le_bytes())?;
        self.writer.write_all(&[1, 0])?; // one section
        self.writer.write_all(&timestamp.to_le_bytes())?;
        self.writer.write_all(&pointer_to_symbol_table.to_le_bytes())?;
        self.writer.write_all(&[2, 0, 0, 0])?; // two symbol table entries
        self.writer.write_all(&[0; 4])?; // no optional header, characteristics 0

        self.writer.write_all(b".rsrc\0\0\0")?;
        self.writer.write_all(&[0; 8])?; // virtual size and address
        self.writer.write_all(&self.size_of_raw_data.to_le_bytes())?;
        self.writer.write_all(&[60, 0, 0, 0])?; // pointer to raw data
        self.writer
            .write_all(&(self.size_of_raw_data + 60).to_le_bytes())?; // relocations
        self.writer.write_all(&[0; 4])?; // line numbers
        self.writer.write_all(&self.number_of_relocations.to_le_bytes())?;
        self.writer.write_all(&[0; 2])?;
        self.writer.write_all(&[0x40, 0, 0x30, 0xc0])?; // characteristics

        self.writer.seek(SeekFrom::Start(end_of_file))?;
        Ok(self.writer)
    }
}

/// IMAGE_RESOURCE_DIRECTORY with only the ID entry count filled in.
pub fn resource_directory_table(number_of_id_entries: u16) -> [u8; 16] {
    let mut table = [0; 16];
    table[14..16].copy_from_slice(&number_of_id_entries.to_le_bytes());
    table
}

/// IMAGE_RESOURCE_DIRECTORY_ENTRY for a numeric ID.
pub fn resource_directory_id_entry(id: u32, offset: u32, subdirectory: bool) -> [u8; 8] {
    let mut entry = [0; 8];
    entry[0..4].copy_from_slice(&id.to_le_bytes());
    let flag: u32 = if subdirectory { 0x80000000 } else { 0 };
    entry[4..8].copy_from_slice(&((offset & 0x7fffffff) | flag).to_le_bytes());
    entry
}

/// IMAGE_RESOURCE_DATA_ENTRY; the RVA field gets a section relocation.
pub fn resource_data_entry(rva: u32, size: u32) -> [u8; 16] {
    let mut entry = [0; 16];
    entry[0..4].copy_from_slice(&rva.to_le_bytes());
    entry[4..8].copy_from_slice(&size.to_le_bytes());
    entry
}
