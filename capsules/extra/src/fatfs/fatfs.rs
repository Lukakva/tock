use core::cell::Cell;

use byteorder::{ByteOrder, LittleEndian};
use kernel::utilities::cells::{MapCell, OptionalCell};
use kernel::ErrorCode;

use crate::fatfs::{
    fs::{parse_volume, RESERVED_ENTRIES},
    utils::{
        Block, BlockCount, BlockIdx, Error, PARTITION_ID_FAT16, PARTITION_ID_FAT16_LBA,
        PARTITION_ID_FAT32_CHS_LBA, PARTITION_ID_FAT32_LBA,
    },
};

use super::{
    fs::FatType,
    utils::{
        AsyncBlockDevice, AsyncBlockDeviceClient, Attributes, Cluster, DirEntry, Directory, File,
        Mode, ShortFileName, TimeSource, Volume, VolumeIdx, VolumeType, MAX_FILE_SIZE,
    },
};

const MAX_DIRS: usize = 8;
const MAX_FILES: usize = 8;

/// Stores the last state that the controller was in, before an async
/// call was made to read a block from the underlying device.
#[derive(Debug, PartialEq)]
pub enum CurrentAsyncOperation {
    Idle,
    GetVolume {
        volume_idx: u32,
    },
    // An operation was launched to find the directory entry.
    // This operation is long since it goes through clusters to find the entry.
    FindDirectoryEntryFat32 {
        name: ShortFileName,
        current_cluster: u32,
        last_cluster: u32,
    },
}

pub trait FatFsClient {
    /// Callback for when the `Controller.get_volume()` operation finishes.
    fn on_get_volume_done(&self, result: Result<Volume, Error>);
}

/// A FatFs controller. The main item.
pub struct FatFs<'a, T, const MAX_DIRS: usize = 4, const MAX_FILES: usize = 4>
where
    T: TimeSource,
{
    client: OptionalCell<&'static dyn FatFsClient>,
    state: MapCell<CurrentAsyncOperation>,
    buffer: &'static mut [u8; 512],
    pub(crate) block_device: &'a dyn AsyncBlockDevice<'a>,
    pub(crate) timesource: T,
    // Values below keep track of userspace requests and are not part of
    // initializing the driver, therefore they are optional.
    /// The current volume (partition) the driver is working in.
    volumes: [Option<Volume>; 4],
    open_dirs: [(VolumeIdx, Cluster); MAX_DIRS],
    open_files: [(VolumeIdx, Cluster); MAX_FILES],
}

impl<'a, T, const MAX_DIRS: usize, const MAX_FILES: usize> FatFs<'a, T, MAX_DIRS, MAX_FILES>
where
    T: TimeSource,
{
    /// Create a new Disk Controller using a generic `BlockDevice`. From this
    /// controller we can open volumes (partitions) and with those we can open
    /// files.
    pub fn new(
        block_device: &'a dyn AsyncBlockDevice<'a>,
        timesource: T,
        buffer: &'a mut [u8; 512],
    ) -> FatFs<'a, T, MAX_DIRS, MAX_FILES> {
        FatFs {
            state: MapCell::empty(),
            buffer,
            client: OptionalCell::default(),
            block_device,
            timesource,
            volumes: [None, None, None, None],
            open_dirs: [(VolumeIdx(0), Cluster::INVALID); MAX_DIRS],
            open_files: [(VolumeIdx(0), Cluster::INVALID); MAX_FILES],
        }
    }

    /// Starts the retrieval process of a volume.
    pub fn get_volume(&mut self, volume_idx: VolumeIdx) -> Result<(), ErrorCode> {
        self.state.map_or(Err(ErrorCode::FAIL), |state| {
            if *state != CurrentAsyncOperation::Idle {
                return Err(ErrorCode::BUSY);
            }

            *state = CurrentAsyncOperation::GetVolume {
                volume_idx: volume_idx.0 as u32,
            };

            self.block_device.start_read(self.buffer, 0)
        })
    }

    /// Actual logic of retrieving the volume, after read is done.
    fn get_volume_resume(&mut self, volume_idx: VolumeIdx) -> Result<Volume, Error> {
        const PARTITION1_START: usize = 446;
        const PARTITION2_START: usize = PARTITION1_START + PARTITION_INFO_LENGTH;
        const PARTITION3_START: usize = PARTITION2_START + PARTITION_INFO_LENGTH;
        const PARTITION4_START: usize = PARTITION3_START + PARTITION_INFO_LENGTH;
        const FOOTER_START: usize = 510;
        const FOOTER_VALUE: u16 = 0xAA55;
        const PARTITION_INFO_LENGTH: usize = 16;
        const PARTITION_INFO_STATUS_INDEX: usize = 0;
        const PARTITION_INFO_TYPE_INDEX: usize = 4;
        const PARTITION_INFO_LBA_START_INDEX: usize = 8;
        const PARTITION_INFO_NUM_BLOCKS_INDEX: usize = 12;

        let (part_type, lba_start, num_blocks) = {
            let block = self.buffer.as_ref();
            // We only support Master Boot Record (MBR) partitioned cards, not
            // GUID Partition Table (GPT)
            if LittleEndian::read_u16(&block[FOOTER_START..FOOTER_START + 2]) != FOOTER_VALUE {
                return Err(Error::FormatError);
            }

            let partition = match volume_idx {
                VolumeIdx(0) => {
                    &block[PARTITION1_START..(PARTITION1_START + PARTITION_INFO_LENGTH)]
                }
                VolumeIdx(1) => {
                    &block[PARTITION2_START..(PARTITION2_START + PARTITION_INFO_LENGTH)]
                }
                VolumeIdx(2) => {
                    &block[PARTITION3_START..(PARTITION3_START + PARTITION_INFO_LENGTH)]
                }
                VolumeIdx(3) => {
                    &block[PARTITION4_START..(PARTITION4_START + PARTITION_INFO_LENGTH)]
                }
                _ => {
                    return Err(Error::NoSuchVolume);
                }
            };
            // Only 0x80 and 0x00 are valid (bootable, and non-bootable)
            if (partition[PARTITION_INFO_STATUS_INDEX] & 0x7F) != 0x00 {
                return Err(Error::FormatError);
            }
            let lba_start = LittleEndian::read_u32(
                &partition[PARTITION_INFO_LBA_START_INDEX..(PARTITION_INFO_LBA_START_INDEX + 4)],
            );
            let num_blocks = LittleEndian::read_u32(
                &partition[PARTITION_INFO_NUM_BLOCKS_INDEX..(PARTITION_INFO_NUM_BLOCKS_INDEX + 4)],
            );
            (
                partition[PARTITION_INFO_TYPE_INDEX],
                BlockIdx(lba_start),
                BlockCount(num_blocks),
            )
        };
        match part_type {
            PARTITION_ID_FAT32_CHS_LBA
            | PARTITION_ID_FAT32_LBA
            | PARTITION_ID_FAT16_LBA
            | PARTITION_ID_FAT16 => {
                let volume = parse_volume(self, lba_start, num_blocks)?;
                Ok(Volume {
                    idx: volume_idx,
                    volume_type: volume,
                })
            }
            _ => Err(Error::FormatError),
        }
    }

    /// Open a directory.
    ///
    /// You can then read the directory entries with `iterate_dir` and `open_file_in_dir`.
    ///
    /// TODO: Work out how to prevent damage occuring to the file system while
    /// this directory handle is open. In particular, stop this directory
    /// being unlinked.
    pub fn open_root_dir(&mut self, volume: &Volume) -> Result<Directory, Error> {
        // Find a free directory entry, and check the root dir isn't open. As
        // we already know the root dir's magic cluster number, we can do both
        // checks in one loop.
        let mut open_dirs_row = None;
        for (i, d) in self.open_dirs.iter().enumerate() {
            if *d == (volume.idx, Cluster::ROOT_DIR) {
                return Err(Error::DirAlreadyOpen);
            }
            if d.1 == Cluster::INVALID {
                open_dirs_row = Some(i);
                break;
            }
        }
        let open_dirs_row = open_dirs_row.ok_or(Error::TooManyOpenDirs)?;
        // Remember this open directory
        self.open_dirs[open_dirs_row] = (volume.idx, Cluster::ROOT_DIR);
        Ok(Directory {
            cluster: Cluster::ROOT_DIR,
            entry: None,
        })
    }

    /// Open a directory.
    ///
    /// You can then read the directory entries with `iterate_dir` and `open_file_in_dir`.
    ///
    /// TODO: Work out how to prevent damage occuring to the file system while
    /// this directory handle is open. In particular, stop this directory
    /// being unlinked.
    pub fn open_dir(
        &mut self,
        volume: &Volume,
        parent_dir: &Directory,
        name: &[u8],
    ) -> Result<Directory, Error> {
        // Find a free open directory table row
        let mut open_dirs_row = None;
        for (i, d) in self.open_dirs.iter().enumerate() {
            if d.1 == Cluster::INVALID {
                open_dirs_row = Some(i);
            }
        }
        let open_dirs_row = open_dirs_row.ok_or(Error::TooManyOpenDirs)?;

        // Open the directory
        let dir_entry = match &volume.volume_type {
            VolumeType::Fat(fat) => fat.find_directory_entry(self, parent_dir, name)?,
        };

        if !dir_entry.attributes.is_directory() {
            return Err(Error::OpenedDirAsFile);
        }

        // Check it's not already open
        for (_i, dir_table_row) in self.open_dirs.iter().enumerate() {
            if *dir_table_row == (volume.idx, dir_entry.cluster) {
                return Err(Error::DirAlreadyOpen);
            }
        }
        // Remember this open directory
        self.open_dirs[open_dirs_row] = (volume.idx, dir_entry.cluster);
        Ok(Directory {
            cluster: dir_entry.cluster,
            entry: Some(dir_entry),
        })
    }

    /// Close a directory. You cannot perform operations on an open directory
    /// and so must close it if you want to do something with it.
    pub fn close_dir(&mut self, volume: &Volume, dir: Directory) {
        let target = (volume.idx, dir.cluster);
        for d in self.open_dirs.iter_mut() {
            if *d == target {
                d.1 = Cluster::INVALID;
                break;
            }
        }
        drop(dir);
    }

    /// Look in a directory for a named file.
    pub fn find_directory_entry(
        &mut self,
        volume: &Volume,
        dir: &Directory,
        name: &[u8],
    ) -> Result<DirEntry, Error> {
        match &volume.volume_type {
            VolumeType::Fat(fat) => fat.find_directory_entry(self, dir, name),
        }
    }

    /// Call a callback function for each directory entry in a directory.
    pub fn iterate_dir<F>(&mut self, volume: &Volume, dir: &Directory, func: F) -> Result<(), Error>
    where
        F: FnMut(&DirEntry),
    {
        match &volume.volume_type {
            VolumeType::Fat(fat) => fat.iterate_dir(self, dir, func),
        }
    }

    /// Open a file from DirEntry. This is obtained by calling iterate_dir. A file can only be opened once.
    pub fn open_dir_entry(
        &mut self,
        volume: &mut Volume,
        dir_entry: DirEntry,
        mode: Mode,
    ) -> Result<File, Error> {
        let open_files_row = self.get_open_files_row()?;
        // Check it's not already open
        for dir_table_row in self.open_files.iter() {
            if *dir_table_row == (volume.idx, dir_entry.cluster) {
                return Err(Error::DirAlreadyOpen);
            }
        }
        if dir_entry.attributes.is_directory() {
            return Err(Error::OpenedDirAsFile);
        }
        if dir_entry.attributes.is_read_only() && mode != Mode::ReadOnly {
            return Err(Error::ReadOnly);
        }

        let mode = solve_mode_variant(mode, true);
        let file = match mode {
            Mode::ReadOnly => File {
                starting_cluster: dir_entry.cluster,
                current_cluster: (0, dir_entry.cluster),
                current_offset: 0,
                length: dir_entry.size,
                mode,
                entry: dir_entry,
            },
            Mode::ReadWriteAppend => {
                let mut file = File {
                    starting_cluster: dir_entry.cluster,
                    current_cluster: (0, dir_entry.cluster),
                    current_offset: 0,
                    length: dir_entry.size,
                    mode,
                    entry: dir_entry,
                };
                // seek_from_end with 0 can't fail
                file.seek_from_end(0).ok();
                file
            }
            Mode::ReadWriteTruncate => {
                let mut file = File {
                    starting_cluster: dir_entry.cluster,
                    current_cluster: (0, dir_entry.cluster),
                    current_offset: 0,
                    length: dir_entry.size,
                    mode,
                    entry: dir_entry,
                };
                match &mut volume.volume_type {
                    VolumeType::Fat(fat) => {
                        fat.truncate_cluster_chain(self, file.starting_cluster)?
                    }
                };
                file.update_length(0);
                // TODO update entry Timestamps
                match &volume.volume_type {
                    VolumeType::Fat(fat) => {
                        let fat_type = fat.get_fat_type();
                        self.write_entry_to_disk(fat_type, &file.entry)?;
                    }
                };

                file
            }
            _ => return Err(Error::Unsupported),
        };
        // Remember this open file
        self.open_files[open_files_row] = (volume.idx, file.starting_cluster);
        Ok(file)
    }

    /// Open a file with the given full path. A file can only be opened once.
    pub fn open_file_in_dir(
        &mut self,
        volume: &mut Volume,
        dir: &Directory,
        name: &[u8],
        mode: Mode,
    ) -> Result<File, Error> {
        let dir_entry = match &volume.volume_type {
            VolumeType::Fat(fat) => fat.find_directory_entry(self, dir, name),
        };

        let open_files_row = self.get_open_files_row()?;
        let dir_entry = match dir_entry {
            Ok(entry) => Some(entry),
            Err(_)
                if (mode == Mode::ReadWriteCreate)
                    | (mode == Mode::ReadWriteCreateOrTruncate)
                    | (mode == Mode::ReadWriteCreateOrAppend) =>
            {
                None
            }
            _ => return Err(Error::FileNotFound),
        };

        let mode = solve_mode_variant(mode, dir_entry.is_some());

        match mode {
            Mode::ReadWriteCreate => {
                if dir_entry.is_some() {
                    return Err(Error::FileAlreadyExists);
                }
                let file_name =
                    ShortFileName::create_from_buffer(name).map_err(|x| Error::FilenameError)?;
                let att = Attributes::create_from_fat(0);
                let entry = match &mut volume.volume_type {
                    VolumeType::Fat(fat) => {
                        fat.write_new_directory_entry(self, dir, file_name, att)?
                    }
                };

                let file = File {
                    starting_cluster: entry.cluster,
                    current_cluster: (0, entry.cluster),
                    current_offset: 0,
                    length: entry.size,
                    mode,
                    entry,
                };
                // Remember this open file
                self.open_files[open_files_row] = (volume.idx, file.starting_cluster);
                Ok(file)
            }
            _ => {
                // Safe to unwrap, since we actually have an entry if we got here
                let dir_entry = dir_entry.unwrap();
                // FIXME: if 2 files are in the same cluster this will cause an error when opening
                // a file for a first time in a different than `ReadWriteCreate` mode.
                self.open_dir_entry(volume, dir_entry, mode)
            }
        }
    }

    /// Get the next entry in open_files list
    fn get_open_files_row(&self) -> Result<usize, Error> {
        // Find a free directory entry
        let mut open_files_row = None;
        for (i, d) in self.open_files.iter().enumerate() {
            if d.1 == Cluster::INVALID {
                open_files_row = Some(i);
            }
        }
        open_files_row.ok_or(Error::TooManyOpenDirs)
    }

    /// Delete a closed file with the given full path, if exists.
    pub fn delete_file_in_dir(
        &mut self,
        volume: &Volume,
        dir: &Directory,
        name: &[u8],
    ) -> Result<(), Error> {
        let dir_entry = match &volume.volume_type {
            VolumeType::Fat(fat) => fat.find_directory_entry(self, dir, name),
        }?;

        if dir_entry.attributes.is_directory() {
            return Err(Error::DeleteDirAsFile);
        }

        let target = (volume.idx, dir_entry.cluster);
        for d in self.open_files.iter_mut() {
            if *d == target {
                return Err(Error::FileIsOpen);
            }
        }

        match &volume.volume_type {
            VolumeType::Fat(fat) => fat.delete_directory_entry(self, dir, name),
        }
    }

    /// Read from an open file.
    pub fn read(
        &mut self,
        volume: &Volume,
        file: &mut File,
        buffer: &mut [u8],
    ) -> Result<usize, Error> {
        // Calculate which file block the current offset lies within
        // While there is more to rearead the block and copy in to the buffer.
        // If we need to find the next cluster, walk the FAT.
        let mut space = buffer.len();
        let mut read = 0;
        while space > 0 && !file.eof() {
            let (block_idx, block_offset, block_avail) =
                self.find_data_on_disk(volume, &mut file.current_cluster, file.current_offset)?;
            let mut blocks = [Block::new()];
            // TODO: ASYNC;
            // self.block_device.read(&mut blocks, block_idx, "read")?;
            let block = &blocks[0];
            let to_copy = block_avail.min(space).min(file.left() as usize);
            assert!(to_copy != 0);
            buffer[read..read + to_copy]
                .copy_from_slice(&block[block_offset..block_offset + to_copy]);
            read += to_copy;
            space -= to_copy;
            file.seek_from_current(to_copy as i32).unwrap();
        }
        Ok(read)
    }

    /// Write to a open file.
    pub fn write(
        &mut self,
        volume: &mut Volume,
        file: &mut File,
        buffer: &[u8],
    ) -> Result<usize, Error> {
        #[cfg(feature = "defmt-log")]
        debug!(
            "write(volume={:?}, file={:?}, buffer={:x}",
            volume, file, buffer
        );

        #[cfg(feature = "log")]
        debug!(
            "write(volume={:?}, file={:?}, buffer={:x?}",
            volume, file, buffer
        );

        if file.mode == Mode::ReadOnly {
            return Err(Error::ReadOnly);
        }
        if file.starting_cluster.0 < RESERVED_ENTRIES {
            // file doesn't have a valid allocated cluster (possible zero-length file), allocate one
            file.starting_cluster = match &mut volume.volume_type {
                VolumeType::Fat(fat) => fat.alloc_cluster(self, None, false)?,
            };
            file.entry.cluster = file.starting_cluster;
        }
        if (file.current_cluster.1).0 < file.starting_cluster.0 {
            file.current_cluster = (0, file.starting_cluster);
        }
        let bytes_until_max = usize::try_from(MAX_FILE_SIZE - file.current_offset)
            .map_err(|_| Error::ConversionError)?;
        let bytes_to_write = core::cmp::min(buffer.len(), bytes_until_max);
        let mut written = 0;

        while written < bytes_to_write {
            let mut current_cluster = file.current_cluster;
            let (block_idx, block_offset, block_avail) =
                match self.find_data_on_disk(volume, &mut current_cluster, file.current_offset) {
                    Ok(vars) => vars,
                    Err(Error::EndOfFile) => match &mut volume.volume_type {
                        VolumeType::Fat(ref mut fat) => {
                            if fat
                                .alloc_cluster(self, Some(current_cluster.1), false)
                                .is_err()
                            {
                                return Ok(written);
                            }
                            let new_offset = self
                                .find_data_on_disk(
                                    volume,
                                    &mut current_cluster,
                                    file.current_offset,
                                )
                                .map_err(|_| Error::AllocationError)?;
                            new_offset
                        }
                    },
                    Err(e) => return Err(e),
                };
            let mut blocks = [Block::new()];
            let to_copy = core::cmp::min(block_avail, bytes_to_write - written);
            if block_offset != 0 {
                // TODO: ASYNC;
                // self.block_device.read(&mut blocks, block_idx, "read")?;
            }
            let block = &mut blocks[0];
            block[block_offset..block_offset + to_copy]
                .copy_from_slice(&buffer[written..written + to_copy]);
            // TODO: ASYNC;
            // self.block_device.write(&blocks, block_idx)?;
            written += to_copy;
            file.current_cluster = current_cluster;
            let to_copy = i32::try_from(to_copy).map_err(|_| Error::ConversionError)?;
            // TODO: Should we do this once when the whole file is written?
            file.update_length(file.length + (to_copy as u32));
            file.seek_from_current(to_copy).unwrap();
            file.entry.attributes.set_archive(true);
            file.entry.mtime = self.timesource.get_timestamp();
            match &mut volume.volume_type {
                VolumeType::Fat(fat) => {
                    fat.update_info_sector(self)?;
                    self.write_entry_to_disk(fat.get_fat_type(), &file.entry)?;
                }
            }
        }
        Ok(written)
    }

    /// Close a file with the given full path.
    pub fn close_file(&mut self, volume: &Volume, file: File) -> Result<(), Error> {
        let target = (volume.idx, file.starting_cluster);
        for d in self.open_files.iter_mut() {
            if *d == target {
                d.1 = Cluster::INVALID;
                break;
            }
        }
        drop(file);
        Ok(())
    }

    /// Check if any files or folders are open.
    pub fn has_open_handles(&self) -> bool {
        !self
            .open_dirs
            .iter()
            .chain(self.open_files.iter())
            .all(|(_, c)| c == &Cluster::INVALID)
    }

    /// This function turns `desired_offset` into an appropriate block to be
    /// read. It either calculates this based on the start of the file, or
    /// from the last cluster we read - whichever is better.
    fn find_data_on_disk(
        &mut self,
        volume: &Volume,
        start: &mut (u32, Cluster),
        desired_offset: u32,
    ) -> Result<(BlockIdx, usize, usize), Error> {
        let bytes_per_cluster = match &volume.volume_type {
            VolumeType::Fat(fat) => fat.bytes_per_cluster(),
        };
        // How many clusters forward do we need to go?
        let offset_from_cluster = desired_offset - start.0;
        let num_clusters = offset_from_cluster / bytes_per_cluster;
        for _ in 0..num_clusters {
            start.1 = match &volume.volume_type {
                VolumeType::Fat(fat) => fat.next_cluster(self, start.1)?,
            };
            start.0 += bytes_per_cluster;
        }
        // How many blocks in are we?
        let offset_from_cluster = desired_offset - start.0;
        assert!(offset_from_cluster < bytes_per_cluster);
        let num_blocks = BlockCount(offset_from_cluster / Block::LEN_U32);
        let block_idx = match &volume.volume_type {
            VolumeType::Fat(fat) => fat.cluster_to_block(start.1),
        } + num_blocks;
        let block_offset = (desired_offset % Block::LEN_U32) as usize;
        let available = Block::LEN - block_offset;
        Ok((block_idx, block_offset, available))
    }

    /// Writes a Directory Entry to the disk
    fn write_entry_to_disk(self, fat_type: FatType, entry: &DirEntry) -> Result<(), Error> {
        let mut blocks = [Block::new()];
        // TODO: ASYNC;
        // self.block_device.read(&mut blocks, entry.entry_block, "")?;
        let block = &mut blocks[0];

        let start = usize::try_from(entry.entry_offset).map_err(|_| Error::ConversionError)?;
        block[start..start + 32].copy_from_slice(&entry.serialize(fat_type)[..]);

        // TODO: ASYNC;
        // self.block_device.write(&blocks, entry.entry_block)?;
        Ok(())
    }
}

/// Transform mode variants (ReadWriteCreate_Or_Append) to simple modes ReadWriteAppend or
/// ReadWriteCreate
fn solve_mode_variant(mode: Mode, dir_entry_is_some: bool) -> Mode {
    let mut mode = mode;
    if mode == Mode::ReadWriteCreateOrAppend {
        if dir_entry_is_some {
            mode = Mode::ReadWriteAppend;
        } else {
            mode = Mode::ReadWriteCreate;
        }
    } else if mode == Mode::ReadWriteCreateOrTruncate {
        if dir_entry_is_some {
            mode = Mode::ReadWriteTruncate;
        } else {
            mode = Mode::ReadWriteCreate;
        }
    }
    mode
}

impl<'a, T, const MAX_DIRS: usize, const MAX_FILES: usize> AsyncBlockDeviceClient
    for FatFs<'a, T, MAX_DIRS, MAX_FILES>
where
    T: TimeSource,
{
    fn read_done(&mut self, block: &[u8; 512], block_idx: u32, status: Result<(), ()>) {
        self.state.map(|state| {
            match *state {
                CurrentAsyncOperation::Idle => {
                    // Read done, but we were not waiting for a read operation?
                }
                CurrentAsyncOperation::GetVolume { volume_idx } => {
                    let result = self.get_volume_resume(VolumeIdx(volume_idx as usize));
                    *state = CurrentAsyncOperation::Idle;

                    self.client.map(|client| client.on_get_volume_done(result));
                }
                CurrentAsyncOperation::FindDirectoryEntryFat32 {
                    name,
                    current_cluster,
                    last_cluster,
                } => {}
            }
        });
    }

    fn write_done(&mut self, block: &[u8; 512], block_idx: u32, status: Result<(), ()>) {}
}
