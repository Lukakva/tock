// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2023.

//! Provides a FAT32/FAT16 filesystem driver.
//!
//! Currently the driver can only serve one item

use core::cell::Cell;
use core::marker::PhantomData;

use capsules_core::process_console::Command;
use embedded_sdmmc::{BlockDevice, Controller, Directory, File, TimeSource, Volume, VolumeIdx};
use kernel::grant::{AllowRoCount, AllowRwCount, Grant, UpcallCount};
use kernel::hil;
use kernel::process::Process;
use kernel::processbuffer::{ReadableProcessBuffer, WriteableProcessBuffer};
use kernel::syscall::{CommandReturn, SyscallDriver};
use kernel::utilities::cells::{OptionalCell, TakeCell};
use kernel::{ErrorCode, ProcessId};

/// Syscall driver number.
pub const DRIVER_NUM: usize = capsules_core::driver::NUM::FatFS as usize;

/// Ids for read-only allow buffers
mod ro_allow {
    /// ID of the buffer shared by the user space application which we can read from.
    pub const BUFFER: usize = 0;
    /// The number of allow buffers the kernel stores for this grant
    pub const COUNT: u8 = 1;
}

/// Ids for read-write allow buffers
mod rw_allow {
    /// ID of the buffer shared by the user space application which we can write to.
    pub const BUFFER: usize = 0;
    /// The number of allow buffers the kernel stores for this grant
    pub const COUNT: u8 = 1;
}

/// Available syscalls for this driver.
mod Cmd {
    /// Inits the driver to "work for" some process.
    /// Should also pass the volume identifier that the process wants to use.
    /// Volume identifier should be 0, 1, 2, or 3. Only 4 partitions are supported.
    pub const INIT: usize = 0;
    /// Open a directory. Userspace should be cognisant of the open-dir limit.
    pub const OPEN_DIR: usize = 1;
    /// Opens a file in the specified directory.
    pub const OPEN_FILE: usize = 2;
}

// We're not really storing anything in the Grant segment of a process.
#[derive(Default)]
struct App;

const MAX_DIRS: usize = 8;
const MAX_FILES: usize = 8;

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
    controller: TakeCell<'static, Controller<D, T, MAX_DIRS, MAX_FILES>>,
    /// The current volume (partition) the driver is working in.
    volume: OptionalCell<Volume>,
    // Keep track of the structs returned by the controller.
    // We need those to read files in directories.
    directories: [OptionalCell<Directory>; MAX_DIRS],
    // Keep track of the structs returned by the controller.
    files: [OptionalCell<File>; MAX_FILES],
    current_process: OptionalCell<ProcessId>,
}

impl<D: BlockDevice, T: TimeSource> FatFsDriver<D, T> {
    /// Checks if the process making the syscall has the ability to do so.
    /// Returns Ok() iff the process making the syscall is the one which reserved the driver.
    /// Returns Fail(ErrorCode::RESERVE) if no process has reserved the driver,
    /// or Fail(ErrorCode::BUSY) if another process has reserved the driver.
    #[inline]
    fn process_can_request(&self, process_id: ProcessId) -> Result<(), ErrorCode> {
        self.current_process
            // If no current process is set, indicate that the current calling process can
            // request to reserve the driver.
            .map_or(Err(ErrorCode::RESERVE), |id| match *id == process_id {
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
                for dir in self.directories.iter() {
                    dir.clear();
                }

                for file in self.files.iter() {
                    file.clear();
                }

                self.current_process.clear();
            }
            None => {}
        }
    }

    /// Handles the INIT syscall.
    #[inline]
    fn syscall_init(&self, process_id: ProcessId, partition_index: usize) -> CommandReturn {
        let current_process_id = self.current_process.take();
        match current_process_id {
            // Some process reserved the driver.
            Some(id) => {
                // Put the ID back in the cell.
                self.current_process.replace(id);
                // If this process is the current process that is making the syscall
                // inform the process that it already has a reservation with the driver.
                if id == process_id {
                    CommandReturn::failure(ErrorCode::ALREADY)
                } else {
                    CommandReturn::failure(ErrorCode::BUSY)
                }
            }
            // No reservation.
            None => {
                // No process has a reservation.
                self.controller
                    .map(
                        |controller| match controller.get_volume(VolumeIdx(partition_index)) {
                            Ok(volume) => {
                                self.current_process.replace(process_id);
                                self.volume.replace(volume);
                                CommandReturn::success()
                            }
                            Err(_err) => CommandReturn::failure(ErrorCode::FAIL),
                        },
                    )
                    .unwrap_or(CommandReturn::failure(ErrorCode::FAIL))
            }
        }
    }

    /// Handles the OPEN_DIR syscall.
    #[inline]
    fn syscall_opendir(&self, process_id: ProcessId) {}
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
            // Init syscall.
            // Treat the `r2` value as the ID of the partition that this process
            // wants to work with.
            Cmd::INIT => self.syscall_init(process_id, r2),
            Cmd::OPEN_DIR => self.syscall_opendir(process_id),
            _ => CommandReturn::failure(ErrorCode::NOSUPPORT),
        }
    }

    fn allocate_grant(&self, process_id: ProcessId) -> Result<(), kernel::process::Error> {
        self.grants.enter(process_id, |_, _| {})
    }
}
