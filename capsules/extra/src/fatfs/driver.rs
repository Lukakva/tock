// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2023.

//! Provides a FAT32/FAT16 filesystem driver.
//!
//! Currently the driver can only serve one item
use crate::fatfs::fat::{BlockDevice, Directory, FatFs, File, TimeSource, Volume, VolumeIdx};
use kernel::grant::{AllowRoCount, AllowRwCount, Grant, UpcallCount};
use kernel::processbuffer::{ReadableProcessBuffer, WriteableProcessBuffer};
use kernel::syscall::{CommandReturn, SyscallDriver};
use kernel::utilities::cells::{MapCell, OptionalCell, TakeCell};
use kernel::{
    ErrorCode::{self, FAIL, INVAL, RESERVE},
    ProcessId,
};

use super::fat::Mode;

/// Syscall driver number.
pub const DRIVER_NUM: usize = capsules_core::driver::NUM::FatFS as usize;

/// Ids for read-only allow buffers
mod ro_allow {
    /// User process uses this buffer when it wants to write to a file.
    pub const WRITE_BUFFER: usize = 0;
    /// User process uses this buffer to give the driver the name of
    /// the file it wants to perform the operation on.
    pub const FILE_NAME: usize = 1;
    /// The number of allow buffers the kernel stores for this grant
    pub const COUNT: u8 = 2;
}

/// Ids for read-write allow buffers
mod rw_allow {
    /// ID of the buffer shared by the user space application which we can write to.
    pub const BUFFER: usize = 0;
    /// The number of allow buffers the kernel stores for this grant
    pub const COUNT: u8 = 1;
}

/// Available syscalls for this driver.
mod cmd {
    /// Inits the driver to "work for" some process.
    /// Should also pass the volume identifier that the process wants to use.
    /// Volume identifier should be 0, 1, 2, or 3. Only 4 partitions are supported.
    pub const INIT: usize = 0;
    /// Opens the root directory.
    pub const OPEN_ROOT_DIR: usize = 1;
    /// Open a directory. Userspace should be cognisant of the open-dir limit.
    pub const OPEN_DIR: usize = 2;
    /// Close a directory.
    pub const CLOSE_DIR: usize = 3;
    /// Opens a file in the specified directory.
    pub const OPEN_FILE: usize = 4;
    /// Closes a file.
    pub const CLOSE_FILE: usize = 5;

    /// Tells the driver that the process is done using the driver.
    /// Essentially un-reserves it.
    pub const DONE: usize = 8;
}

// We're not really storing anything in the Grant segment of a process.
#[derive(Default)]
struct App;

const MAX_DIRS: usize = 8;
const MAX_FILES: usize = 8;
pub const FILENAME_BUFFER: [u8; 11] = [0; 11];

/// Struct that stores the state of the Fat32 driver.
/// Stores information about the current process that the driver is serving.
/// As well as state of the underlying device that the driver reading/writing to.
pub struct FatFsDriver<D: BlockDevice + 'static, T: TimeSource + 'static> {
    // fs: fatfs::FileSystem<>,
    grants: Grant<
        App,
        UpcallCount<1>,
        AllowRoCount<{ ro_allow::COUNT }>,
        AllowRwCount<{ rw_allow::COUNT }>,
    >,
    fatfs: TakeCell<'static, FatFs<D, T, MAX_DIRS, MAX_FILES>>,

    filename_buffer: TakeCell<'static, [u8]>,

    // Values below keep track of userspace requests and are not part of
    // initializing the driver, therefore they are optional.
    /// The current volume (partition) the driver is working in.
    volume: MapCell<Volume>,
    /// Directory Descriptor table. Every directory opened by the user process
    /// has a corresponding int ID.
    directories: MapCell<[Option<Directory>; MAX_DIRS]>,
    /// File Descriptor table. Every file opened by the user process
    /// has a corresponding int ID.
    files: MapCell<[Option<File>; MAX_FILES]>,
    /// The process that reserved the driver.
    current_process: OptionalCell<ProcessId>,
}

impl<D: BlockDevice, T: TimeSource> FatFsDriver<D, T> {
    /// Checks if the process making the syscall has the ability to do so.
    /// Returns Ok() iff the process making the syscall is the one which reserved the driver.
    /// Returns Fail(ErrorCode::RESERVE) if no process has reserved the driver,
    /// or Fail(ErrorCode::BUSY) if another process has reserved the driver.
    fn has_reservation(&self, process_id: &ProcessId) -> Result<(), ErrorCode> {
        self.current_process
            // If no current process is set, indicate that the current calling process can
            // request to reserve the driver.
            .map_or(Err(ErrorCode::RESERVE), |id| match id == process_id {
                true => Ok(()),
                // Indicate that another process has reserved the driver.
                false => Err(ErrorCode::BUSY),
            })
    }

    /// Resets the driver. This is done after a process is done using the driver.
    fn reset(&self) {
        self.current_process.clear();
        self.volume.take();

        self.fatfs.map(|fs| {
            self.directories.map(|dirs| {
                for i in 0..dirs.len() {
                    dirs[i] = None;
                }
            });

            self.files.map(|files| {
                for i in 0..files.len() {
                    files[i] = None;
                }
            });

            fs.reset();
        });
    }

    /// Handles the INIT syscall.
    fn syscall_init(&self, process_id: ProcessId, partition_index: usize) -> Result<(), ErrorCode> {
        // Init call is a little unique, but we can still re-use the function below
        match self.has_reservation(&process_id) {
            // If Ok() is returned, the current process has already reserved
            // the driver so INIT syscall makes no sense.
            Ok(_) => Err(ErrorCode::ALREADY),
            Err(code) => match code {
                // Can't init for this process. other process has reserved it.
                ErrorCode::BUSY => Err(code),
                // This error means that no reservation has been made so far, so init can happen.
                ErrorCode::RESERVE => {
                    // No process has a reservation.
                    self.fatfs
                        .map(|fs| match fs.get_volume(VolumeIdx(partition_index)) {
                            Ok(volume) => {
                                self.current_process.replace(process_id);
                                self.volume.replace(volume);
                                Ok(())
                            }
                            Err(_err) => Err(ErrorCode::FAIL),
                        })
                        .unwrap_or(Err(ErrorCode::FAIL))
                }
                // Should never happen.
                code => Err(code),
            },
        }
    }

    /// Opens the root directory and stores it in directory descriptor 0.
    fn syscall_open_root_dir(&self, _process_id: ProcessId) -> Result<u32, ErrorCode> {
        self.directories.map_or(Err(ErrorCode::FAIL), |dirs| {
            // If directory descriptor 0 is already taken, it's root.
            if dirs[0].is_some() {
                return Err(ErrorCode::ALREADY);
            }

            self.volume.map_or(Err(ErrorCode::FAIL), |volume| {
                self.fatfs.map_or(Err(ErrorCode::FAIL), |fs| {
                    match fs.open_root_dir(&volume) {
                        Ok(root_directory) => {
                            dirs[0] = Some(root_directory);
                            // Convention is to send back the file/dir descriptor (int)
                            // of whatever the userspace opened. For root dir it's always 0.
                            Ok(0)
                        }
                        Err(_err) => Err(ErrorCode::FAIL),
                    }
                })
            })
        })
    }

    /// Handles the OPEN_DIR syscall.
    fn syscall_opendir(
        &self,
        process_id: ProcessId,
        parent_dir_id: usize,
    ) -> Result<u32, ErrorCode> {
        // Below is the unpacking hell.
        // We need to have access to:
        // - directories
        // - fs controller
        // - current partition (volume)
        // - the local filename buffer
        // - userspace filename buffer
        // and then we can finally perform the logic.
        self.directories.map_or(Err(FAIL), |dirs| {
            self.fatfs.map_or(Err(FAIL), |fs| {
                self.volume.map_or(Err(FAIL), |volume| {
                    self.filename_buffer.map_or(Err(FAIL), |name| {
                        // Retrieve the file name from userspace.
                        self.grants
                            .enter(process_id, |_, kd| {
                                kd.get_readonly_processbuffer(ro_allow::FILE_NAME)
                                    .and_then(|data| {
                                        data.enter(|data| {
                                            // Check if the dir exists.
                                            let parent_dir = match dirs.get(parent_dir_id) {
                                                Some(cell) => match cell {
                                                    Some(dir) => Ok(dir),
                                                    None => Err(INVAL),
                                                },
                                                None => Err(INVAL),
                                            }?; // Note the ? operator.

                                            // Find the next free directory descriptor.
                                            let new_dir_id = {
                                                let mut i = 0;
                                                loop {
                                                    if i >= dirs.len() {
                                                        break Err(ErrorCode::NOMEM);
                                                    }
                                                    if dirs[i].is_none() {
                                                        break Ok(i);
                                                    }
                                                    i += 1;
                                                }
                                            }?; // Note the ? operator.

                                            // Copy the filename from the userspace.
                                            // Make sure we don't go over the userspace buffer,
                                            // as well as our local buffer.
                                            for (i, byte) in data.iter().enumerate() {
                                                if i >= name.len() {
                                                    break;
                                                }

                                                name[i] = byte.get();
                                            }

                                            match fs.open_dir(volume, parent_dir, name) {
                                                Ok(directory) => {
                                                    dirs[new_dir_id] = Some(directory);
                                                    Ok(new_dir_id as u32)
                                                }
                                                Err(_err) => Err(INVAL),
                                            }
                                        })
                                    })
                                    .unwrap_or(Err(FAIL))
                            })
                            .unwrap_or(Err(RESERVE))
                    })
                })
            })
        })
    }

    /// Handles the CLOSE_DIR syscall.
    fn syscall_close_dir(&self, _process_id: ProcessId, dir_id: usize) -> Result<(), ErrorCode> {
        self.directories.map_or(Err(FAIL), |dirs| {
            self.fatfs.map_or(Err(FAIL), |fs| {
                self.volume.map_or(Err(FAIL), |volume| {
                    // Check if the dir exists. If it does, take it out of the memory
                    // and replace it with None.
                    let directory = match dirs.get_mut(dir_id) {
                        Some(cell) => match cell.take() {
                            Some(dir) => Ok(dir),
                            // Not in the array of open dirs. Not open = Closed already.
                            None => Err(ErrorCode::ALREADY),
                        },
                        None => Err(INVAL),
                    }?; // Note the ? operator.

                    fs.close_dir(volume, directory);
                    Ok(())
                })
            })
        })
    }

    /// Handles the OPEN_FILE syscall.
    fn syscall_open_file(
        &self,
        process_id: ProcessId,
        parent_dir_id: usize,
        mode: usize,
    ) -> Result<u32, ErrorCode> {
        // Below is the unpacking hell.
        // We need to have access to:
        // - directories
        // - files
        // - fs controller
        // - current partition (volume)
        // - the local filename buffer
        // - userspace filename buffer
        // and then we can finally perform the logic.
        self.directories.map_or(Err(INVAL), |dirs| {
            self.files.map_or(Err(INVAL), |files| {
                self.fatfs.map_or(Err(INVAL), |fs| {
                    self.volume.map_or(Err(FAIL), |volume| {
                        self.filename_buffer.map_or(Err(FAIL), |name| {
                            // Retrieve the file name from userspace.
                            self.grants
                                .enter(process_id, |_, kd| {
                                    kd.get_readonly_processbuffer(ro_allow::FILE_NAME)
                                        .and_then(|data| {
                                            data.enter(|data| {
                                                // Check if the dir exists.
                                                let parent_dir = match dirs.get(parent_dir_id) {
                                                    Some(cell) => match cell {
                                                        Some(dir) => Ok(dir),
                                                        None => Err(INVAL),
                                                    },
                                                    None => Err(INVAL),
                                                }?; // Note the ? operator.

                                                // Find the next free file descriptor.
                                                let new_file_id = {
                                                    let mut i = 0;
                                                    loop {
                                                        if i >= files.len() {
                                                            break Err(ErrorCode::NOMEM);
                                                        }
                                                        if files[i].is_none() {
                                                            break Ok(i);
                                                        }
                                                        i += 1;
                                                    }
                                                }?; // Note the ? operator.

                                                // Copy the filename from the userspace.
                                                // Make sure we don't go over the userspace buffer,
                                                // as well as our local buffer.
                                                for (i, byte) in data.iter().enumerate() {
                                                    if i >= name.len() {
                                                        break;
                                                    }

                                                    name[i] = byte.get();
                                                }

                                                let mode_enum = match mode {
                                                    0 => Ok(Mode::ReadOnly),
                                                    1 => Ok(Mode::ReadWriteAppend),
                                                    2 => Ok(Mode::ReadWriteTruncate),
                                                    3 => Ok(Mode::ReadWriteCreate),
                                                    4 => Ok(Mode::ReadWriteCreateOrTruncate),
                                                    5 => Ok(Mode::ReadWriteCreateOrAppend),
                                                    _ => Err(ErrorCode::INVAL),
                                                }?;

                                                match fs.open_file_in_dir(
                                                    volume, parent_dir, name, mode_enum,
                                                ) {
                                                    Ok(file) => {
                                                        files[new_file_id] = Some(file);
                                                        Ok(new_file_id as u32)
                                                    }
                                                    Err(_err) => Err(INVAL),
                                                }
                                            })
                                        })
                                        .unwrap_or(Err(FAIL))
                                })
                                .unwrap_or(Err(RESERVE))
                        })
                    })
                })
            })
        })
    }

    /// Handles the CLOSE_FILE syscall.
    fn syscall_close_file(&self, _process_id: ProcessId, file_id: usize) -> Result<(), ErrorCode> {
        self.files.map_or(Err(FAIL), |files| {
            self.fatfs.map_or(Err(FAIL), |fs| {
                self.volume.map_or(Err(FAIL), |volume| {
                    let file = match files.get_mut(file_id) {
                        Some(cell) => match cell.take() {
                            Some(file) => Ok(file),
                            // Not in the array of open files. Not open = Closed already.
                            None => Err(ErrorCode::ALREADY),
                        },
                        None => Err(INVAL),
                    }?; // Note the ? operator.

                    fs.close_file(volume, file);
                    Ok(())
                })
            })
        })
    }
}

impl<D: BlockDevice, T: TimeSource> SyscallDriver for FatFsDriver<D, T> {
    fn command(
        &self,
        command_num: usize,
        r2: usize,
        r3: usize,
        process_id: ProcessId,
    ) -> CommandReturn {
        match command_num {
            // Init syscall. Reserves the driver, if successful.
            // Treat the `r2` value as the ID of the partition that this process
            // wants to work with.
            cmd::INIT => match self.syscall_init(process_id, r2) {
                Ok(_) => CommandReturn::success(),
                Err(code) => CommandReturn::failure(code),
            },
            cmd::OPEN_DIR | cmd::OPEN_ROOT_DIR | cmd::OPEN_FILE | cmd::DONE => {
                // Otherwise, for any other syscall, there needs to be a reservation
                // made by the process.
                match self.has_reservation(&process_id) {
                    Ok(_) => match command_num {
                        cmd::OPEN_ROOT_DIR => match self.syscall_open_root_dir(process_id) {
                            Ok(id) => CommandReturn::success_u32(id),
                            Err(code) => CommandReturn::failure(code),
                        },
                        cmd::OPEN_DIR => match self.syscall_opendir(process_id, r2) {
                            Ok(id) => CommandReturn::success_u32(id),
                            Err(code) => CommandReturn::failure(code),
                        },
                        cmd::CLOSE_DIR => match self.syscall_close_dir(process_id, r2) {
                            Ok(_) => CommandReturn::success(),
                            Err(code) => CommandReturn::failure(code),
                        },
                        cmd::OPEN_FILE => match self.syscall_open_file(process_id, r2, r3) {
                            Ok(id) => CommandReturn::success_u32(id),
                            Err(code) => CommandReturn::failure(code),
                        },
                        cmd::CLOSE_FILE => match self.syscall_close_file(process_id, r2) {
                            Ok(_) => CommandReturn::success(),
                            Err(code) => CommandReturn::failure(code),
                        },
                        _ => CommandReturn::failure(ErrorCode::NOSUPPORT),
                    },
                    Err(code) => CommandReturn::failure(code),
                }
            }
            _ => CommandReturn::failure(ErrorCode::NOSUPPORT),
        }
    }

    fn allocate_grant(&self, process_id: ProcessId) -> Result<(), kernel::process::Error> {
        self.grants.enter(process_id, |_, _| {})
    }
}
