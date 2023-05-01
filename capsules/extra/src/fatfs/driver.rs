// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2023.

//! Provides a FAT32/FAT16 filesystem driver.
//!
//! Currently the driver can only serve one item
use crate::fatfs::fat::{Controller, Directory, File, TimeSource, Volume};
use kernel::grant::{AllowRoCount, AllowRwCount, Grant, UpcallCount};
use kernel::processbuffer::ReadableProcessBuffer;
use kernel::syscall::{CommandReturn, SyscallDriver};
use kernel::utilities::cells::{MapCell, OptionalCell, TakeCell};
use kernel::{ErrorCode, ProcessId};

use super::fat::blockdevice::AsyncBlockDevice;
use super::fat::ControllerClient;

/// Syscall driver number.
pub const DRIVER_NUM: usize = capsules_core::driver::NUM::FatFS as usize;

pub const BLOCK_SIZE: usize = 512;

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
    /// Opens a file in the specified directory.
    pub const OPEN_FILE: usize = 3;

    /// Tells the driver that the process is done using the driver.
    /// Essentially un-reserves it.
    pub const DONE: usize = 8;
}

// We're not really storing anything in the Grant segment of a process.
#[derive(Default)]
struct App;

const MAX_DIRS: usize = 8;
const MAX_FILES: usize = 8;

pub enum CurrentAsyncOperation {
    // The file system is not doing anything. Not waiting for any responses
    // from the block device.
    None,
    Waiting,
}

/// A Fat file system, mounted on top of some `AsyncBlockDevice`.
/// Can only serve 1 entity at a time (process, another driver).
pub struct FatFs<D: AsyncBlockDevice<'static> + 'static, T: TimeSource + 'static> {
    grants: Grant<
        App,
        UpcallCount<1>,
        AllowRoCount<{ ro_allow::COUNT }>,
        AllowRwCount<{ rw_allow::COUNT }>,
    >,
    controller: TakeCell<'static, Controller<'static, D, T, MAX_DIRS, MAX_FILES>>,

    filename_buffer: TakeCell<'static, str>,

    // Values below keep track of userspace requests and are not part of
    // initializing the driver, therefore they are optional.
    /// The current volume (partition) the driver is working in.
    volume: MapCell<Volume>,
    /// Directory Descriptor table. Every directory opened by the user process
    /// has a corresponding int ID.
    directories: [OptionalCell<Directory>; MAX_DIRS],
    /// File Descriptor table. Every file opened by the user process
    /// has a corresponding int ID.
    files: [OptionalCell<File>; MAX_FILES],
    /// The process that reserved the driver.
    current_process: OptionalCell<ProcessId>,
}

impl<'a, D: AsyncBlockDevice<'static>, T: TimeSource> ControllerClient for FatFs<D, T> {
    fn on_get_volume_done(&self, result: Result<Volume, super::fat::Error>) {}
}

impl<'a, D: AsyncBlockDevice<'static>, T: TimeSource> FatFs<D, T> {
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

        // Maybe the process reserved the driver, but didn't do anything.
        match self.volume.take() {
            Some(volume) => {
                self.controller.map(|controller| {
                    for dir in self.directories.iter() {
                        // Using .take().map() allows us to clear the `OptionalCell` and
                        // at the same time, if there was a directory struct in the cell,
                        // use the value.
                        dir.take().map(|dir| {
                            controller.close_dir(&volume, dir);
                        });
                    }

                    for file in self.files.iter() {
                        file.take().map(|file| {
                            controller.close_file(&volume, file).ok();
                        });
                    }

                    self.current_process.clear();
                });
            }
            None => {}
        };
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
                    self.controller
                        .map(
                            |controller| match controller.get_volume(partition_index as u32) {
                                Ok(volume) => {
                                    self.current_process.replace(process_id);
                                    self.volume.replace(volume);
                                    Ok(())
                                }
                                Err(_err) => Err(ErrorCode::FAIL),
                            },
                        )
                        .unwrap_or(Err(ErrorCode::FAIL))
                }
                // Should never happen.
                code => Err(code),
            },
        }
    }

    /// Opens the root directory and stores it in directory descriptor 0.
    fn syscall_open_root_dir(&self, process_id: ProcessId) -> Result<u32, ErrorCode> {
        // If directory descriptor 0 is already taken, it's root.
        if self.directories[0].is_some() {
            return Err(ErrorCode::ALREADY);
        }

        self.volume.take().map_or(Err(ErrorCode::FAIL), |volume| {
            let result = self
                .controller
                .map(|controller| match controller.open_root_dir(&volume) {
                    Ok(root_directory) => {
                        self.directories[0].replace(root_directory);
                        // Convention is to send back the file/dir descriptor (int)
                        // of whatever the userspace opened. For root dir it's always 0.
                        Ok(0)
                    }
                    Err(err) => Err(ErrorCode::FAIL),
                })
                .unwrap_or(Err(ErrorCode::FAIL));

            // Put back the volume struct.
            self.volume.replace(volume);

            return result;
        })
    }

    /// Handles the OPEN_DIR syscall.
    fn syscall_opendir(
        &self,
        process_id: ProcessId,
        parent_dir_id: usize,
    ) -> Result<u32, ErrorCode> {
        // Below is the unpacking hell. Essentially this code, in a safe manner
        // 1. Ensures the directory ID given by the process is valid.
        // 2. Retrieves the controller.
        // 3. Retrieves the current volume the process is working with.
        // 4. Retrieves the string slice from the user process that contains
        // the name of the dir.
        self.directories
            .get(parent_dir_id)
            .map_or(Err(ErrorCode::INVAL), |cell| {
                cell.take().map_or(Err(ErrorCode::INVAL), |parent_dir| {
                    self.controller.map_or(Err(ErrorCode::FAIL), |controller| {
                        self.volume.take().map_or(Err(ErrorCode::FAIL), |volume| {
                            // Retrieve the file name from userspace.
                            let name = self.grants.enter(process_id, |app_data, kernel_data| {
                                // kernel_data.get_readonly_processbuffer(ro_allow::FILE_NAME)
                                // .and_then(|data| data.enter(|data| data.cop))
                            });

                            let result = match controller.open_dir(&volume, &parent_dir, "asd") {
                                Ok(res) => Ok(1),
                                Err(err) => Err(ErrorCode::INVAL),
                            };

                            // Put back
                            self.volume.replace(volume);
                            result
                        })
                    })
                })
            })
    }

    /// Handles the OPEN_FILE syscall.
    fn syscall_open_file() {}
}

impl<'a, D: AsyncBlockDevice<'static>, T: TimeSource> SyscallDriver for FatFs<D, T> {
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
            command => {
                // Otherwise, for any other syscall, there needs to be a reservation
                // made by the process.
                match self.has_reservation(&process_id) {
                    Ok(_) => match command {
                        cmd::OPEN_ROOT_DIR => match self.syscall_open_root_dir(process_id) {
                            Ok(id) => CommandReturn::success_u32(id),
                            Err(code) => CommandReturn::failure(code),
                        },
                        cmd::OPEN_DIR => match self.syscall_opendir(process_id, r2) {
                            Ok(id) => CommandReturn::success_u32(id),
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
