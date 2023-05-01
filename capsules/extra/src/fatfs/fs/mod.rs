//! embedded-sdmmc-rs - FAT16/FAT32 file system implementation
//!
//! Implements the File Allocation Table file system. Supports FAT16 and FAT32 volumes.

/// Number of entries reserved at the start of a File Allocation Table
pub const RESERVED_ENTRIES: u32 = 2;

/// Indentifies the supported types of FAT format
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FatType {
    /// FAT16 Format
    Fat16,
    /// FAT32 Format
    Fat32,
}

mod bpb;
mod info;
mod ondiskdirentry;
mod volume;

pub use bpb::Bpb;
pub use info::{Fat16Info, Fat32Info, FatSpecificInfo, InfoSector};
pub use ondiskdirentry::OnDiskDirEntry;
pub use volume::{parse_volume, FatVolume, VolumeName};
