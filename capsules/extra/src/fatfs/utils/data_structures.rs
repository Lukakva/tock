//! embedded-sdmmc-rs - Block Device support
//!
//! Generic code for handling block devices.

use super::super::fs::FatVolume;
use kernel::ErrorCode;

/// Represents a standard 512 byte block (also known as a sector). IBM PC
/// formatted 5.25" and 3.5" floppy disks, SD/MMC cards up to 1 GiB in size
/// and IDE/SATA Hard Drives up to about 2 TiB all have 512 byte blocks.
///
/// This library does not support devices with a block size other than 512
/// bytes.
#[derive(Clone)]
pub struct Block {
    /// The 512 bytes in this block (or sector).
    pub contents: [u8; Block::LEN],
}

/// Represents the linear numeric address of a block (or sector). The first
/// block on a disk gets `BlockIdx(0)` (which usually contains the Master Boot
/// Record).
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockIdx(pub u32);

/// Represents the a number of blocks (or sectors). Add this to a `BlockIdx`
/// to get an actual address on disk.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockCount(pub u32);

/// An iterator returned from `Block::range`.
pub struct BlockIter {
    inclusive_end: BlockIdx,
    current: BlockIdx,
}

pub trait AsyncBlockDevice<'a> {
    /// Schedule an async read operation of one block.
    fn start_read(&self, block: &'static mut [u8; 512], block_idx: u32) -> Result<(), ErrorCode>;
    /// Schedule an async write operation of one block.
    fn start_write(&self, block: &'static [u8; 512], block_idx: u32) -> Result<(), ErrorCode>;
    /// Returns the client who is listening for this device.
    fn get_client(&'a self) -> Option<&'a dyn AsyncBlockDeviceClient>;
    fn set_client(&'a self, client: &'a dyn AsyncBlockDeviceClient);
    // /// Determine how many blocks this device can hold.
    // fn num_blocks(&self) -> Result<u32, Error>;
}

pub trait AsyncBlockDeviceClient {
    fn read_done(&mut self, block: &[u8; 512], block_idx: u32, status: Result<(), ()>);
    fn write_done(&mut self, block: &[u8; 512], block_idx: u32, status: Result<(), ()>);
}

impl Block {
    /// All our blocks are a fixed length of 512 bytes. We do not support
    /// 'Advanced Format' Hard Drives with 4 KiB blocks, nor weird old
    /// pre-3.5-inch floppy disk formats.
    pub const LEN: usize = 512;

    /// Sometimes we want `LEN` as a `u32` and the casts don't look nice.
    pub const LEN_U32: u32 = 512;

    /// Create a new block full of zeros.
    pub fn new() -> Block {
        Block {
            contents: [0u8; Self::LEN],
        }
    }
}

impl Default for Block {
    fn default() -> Self {
        Self::new()
    }
}

impl core::ops::Add<BlockCount> for BlockIdx {
    type Output = BlockIdx;
    fn add(self, rhs: BlockCount) -> BlockIdx {
        BlockIdx(self.0 + rhs.0)
    }
}

impl core::ops::AddAssign<BlockCount> for BlockIdx {
    fn add_assign(&mut self, rhs: BlockCount) {
        self.0 += rhs.0
    }
}

impl core::ops::Add<BlockCount> for BlockCount {
    type Output = BlockCount;
    fn add(self, rhs: BlockCount) -> BlockCount {
        BlockCount(self.0 + rhs.0)
    }
}

impl core::ops::AddAssign<BlockCount> for BlockCount {
    fn add_assign(&mut self, rhs: BlockCount) {
        self.0 += rhs.0
    }
}

impl core::ops::Sub<BlockCount> for BlockIdx {
    type Output = BlockIdx;
    fn sub(self, rhs: BlockCount) -> BlockIdx {
        BlockIdx(self.0 - rhs.0)
    }
}

impl core::ops::SubAssign<BlockCount> for BlockIdx {
    fn sub_assign(&mut self, rhs: BlockCount) {
        self.0 -= rhs.0
    }
}

impl core::ops::Sub<BlockCount> for BlockCount {
    type Output = BlockCount;
    fn sub(self, rhs: BlockCount) -> BlockCount {
        BlockCount(self.0 - rhs.0)
    }
}

impl core::ops::SubAssign<BlockCount> for BlockCount {
    fn sub_assign(&mut self, rhs: BlockCount) {
        self.0 -= rhs.0
    }
}

impl core::ops::Deref for Block {
    type Target = [u8; 512];
    fn deref(&self) -> &[u8; 512] {
        &self.contents
    }
}

impl core::ops::DerefMut for Block {
    fn deref_mut(&mut self) -> &mut [u8; 512] {
        &mut self.contents
    }
}

impl core::fmt::Debug for Block {
    fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
        writeln!(fmt, "Block:")?;
        for line in self.contents.chunks(32) {
            for b in line {
                write!(fmt, "{:02x}", b)?;
            }
            write!(fmt, " ")?;
            for &b in line {
                if (0x20..=0x7F).contains(&b) {
                    write!(fmt, "{}", b as char)?;
                } else {
                    write!(fmt, ".")?;
                }
            }
            writeln!(fmt)?;
        }
        Ok(())
    }
}

impl BlockIdx {
    /// Convert a block index into a 64-bit byte offset from the start of the
    /// volume. Useful if your underlying block device actually works in
    /// bytes, like `open("/dev/mmcblk0")` does on Linux.
    pub fn into_bytes(self) -> u64 {
        (u64::from(self.0)) * (Block::LEN as u64)
    }

    /// Create an iterator from the current `BlockIdx` through the given
    /// number of blocks.
    pub fn range(self, num: BlockCount) -> BlockIter {
        BlockIter::new(self, self + BlockCount(num.0))
    }
}

impl BlockCount {
    /// Take a number of blocks and increment by the integer number of blocks
    /// required to get to the block that holds the byte at the given offset.
    pub fn offset_bytes(self, offset: u32) -> Self {
        BlockCount(self.0 + (offset / Block::LEN_U32))
    }
}

impl BlockIter {
    /// Create a new `BlockIter`, from the given start block, through (and
    /// including) the given end block.
    pub fn new(start: BlockIdx, inclusive_end: BlockIdx) -> BlockIter {
        BlockIter {
            inclusive_end,
            current: start,
        }
    }
}

impl core::iter::Iterator for BlockIter {
    type Item = BlockIdx;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current.0 >= self.inclusive_end.0 {
            None
        } else {
            let this = self.current;
            self.current += BlockCount(1);
            Some(this)
        }
    }
}

/// Represents all the ways the functions in this crate can fail.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, Clone)]
pub enum Error {
    /// The underlying block device threw an error.
    DeviceError,
    /// Device is busy.
    DeviceBusy,
    /// The filesystem is badly formatted (or this code is buggy).
    FormatError,
    /// The given `VolumeIdx` was bad,
    NoSuchVolume,
    /// The given filename was bad
    FilenameError,
    /// Out of memory opening directories
    TooManyOpenDirs,
    /// Out of memory opening files
    TooManyOpenFiles,
    /// That file doesn't exist
    FileNotFound,
    /// You can't open a file twice
    FileAlreadyOpen,
    /// You can't open a directory twice
    DirAlreadyOpen,
    /// You can't open a directory as a file
    OpenedDirAsFile,
    /// You can't delete a directory as a file
    DeleteDirAsFile,
    /// You can't delete an open file
    FileIsOpen,
    /// We can't do that yet
    Unsupported,
    /// Tried to read beyond end of file
    EndOfFile,
    /// Found a bad cluster
    BadCluster,
    /// Error while converting types
    ConversionError,
    /// The device does not have enough space for the operation
    NotEnoughSpace,
    /// Cluster was not properly allocated by the library
    AllocationError,
    /// Jumped to free space during fat traversing
    JumpedFree,
    /// Tried to open Read-Only file with write mode
    ReadOnly,
    /// Tried to create an existing file
    FileAlreadyExists,
    /// Bad block size - only 512 byte blocks supported
    BadBlockSize(u16),
    /// Entry not found in the block
    NotInBlock,
}

/// Represents a partition with a filesystem within it.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub struct Volume {
    pub(crate) idx: VolumeIdx,
    pub(crate) volume_type: VolumeType,
}

/// This enum holds the data for the various different types of filesystems we
/// support.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub enum VolumeType {
    /// FAT16/FAT32 formatted volumes.
    Fat(FatVolume),
}

/// A `VolumeIdx` is a number which identifies a volume (or partition) on a
/// disk. `VolumeIdx(0)` is the first primary partition on an MBR partitioned
/// disk.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct VolumeIdx(pub usize);

// ****************************************************************************
//
// Public Data
//
// ****************************************************************************

// None

// ****************************************************************************
//
// Private Types
//
// ****************************************************************************

/// Marker for a FAT32 partition. Sometimes also use for FAT16 formatted
/// partitions.
pub const PARTITION_ID_FAT32_LBA: u8 = 0x0C;
/// Marker for a FAT16 partition with LBA. Seen on a Raspberry Pi SD card.
pub const PARTITION_ID_FAT16_LBA: u8 = 0x0E;
/// Marker for a FAT16 partition. Seen on a card formatted with the official
/// SD-Card formatter.
pub const PARTITION_ID_FAT16: u8 = 0x06;
/// Marker for a FAT32 partition. What Macosx disk utility (and also SD-Card formatter?)
/// use.
pub const PARTITION_ID_FAT32_CHS_LBA: u8 = 0x0B;

// ****************************************************************************
//
// End Of File
//
// ****************************************************************************
