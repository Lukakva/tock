//! embedded-sdmmc-rs - Generic File System structures
//!
//! Implements generic file system components. These should be applicable to
//! most (if not all) supported filesystems.

/// Maximum file size supported by this library
pub const MAX_FILE_SIZE: u32 = core::u32::MAX;

mod attributes;
mod cluster;
mod data_structures;
mod directory;
mod filename;
mod files;
mod timestamp;

pub use self::attributes::*;
pub use self::cluster::*;
pub use self::data_structures::*;
pub use self::directory::*;
pub use self::filename::*;
pub use self::files::*;
pub use self::timestamp::*;
