//! embedded-sdmmc-rs - Constants from the SD Specifications
//!
//! Based on SdFat, under the following terms:
//!
//! > Copyright (c) 2011-2018 Bill Greiman
//! > This file is part of the SdFat library for SD memory cards.
//! >
//! > MIT License
//! >
//! > Permission is hereby granted, free of charge, to any person obtaining a
//! > copy of this software and associated documentation files (the "Software"),
//! > to deal in the Software without restriction, including without limitation
//! > the rights to use, copy, modify, merge, publish, distribute, sublicense,
//! > and/or sell copies of the Software, and to permit persons to whom the
//! > Software is furnished to do so, subject to the following conditions:
//! >
//! > The above copyright notice and this permission notice shall be included
//! > in all copies or substantial portions of the Software.
//! >
//! > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! > OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! > FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! > AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! > LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! > FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! > DEALINGS IN THE SOFTWARE.

//==============================================================================

// Possible errors the SD card can return

/// Card indicates last operation was a success
pub const ERROR_OK: u8 = 0x00;

//==============================================================================

// SD Card Commands

/// GO_IDLE_STATE - init card in spi mode if CS low
pub const CMD0: u8 = 0x00;
/// SEND_IF_COND - verify SD Memory Card interface operating condition.*/
pub const CMD8: u8 = 0x08;
/// SEND_CSD - read the Card Specific Data (CSD register)
pub const CMD9: u8 = 0x09;
/// STOP_TRANSMISSION - end multiple block read sequence
pub const CMD12: u8 = 0x0C;
/// SEND_STATUS - read the card status register
pub const CMD13: u8 = 0x0D;
/// READ_SINGLE_BLOCK - read a single data block from the card
pub const CMD17: u8 = 0x11;
/// READ_MULTIPLE_BLOCK - read a multiple data blocks from the card
pub const CMD18: u8 = 0x12;
/// WRITE_BLOCK - write a single data block to the card
pub const CMD24: u8 = 0x18;
/// WRITE_MULTIPLE_BLOCK - write blocks of data until a STOP_TRANSMISSION
pub const CMD25: u8 = 0x19;
/// APP_CMD - escape for application specific command
pub const CMD55: u8 = 0x37;
/// READ_OCR - read the OCR register of a card
pub const CMD58: u8 = 0x3A;
/// CRC_ON_OFF - enable or disable CRC checking
pub const CMD59: u8 = 0x3B;
/// SD_SEND_OP_COMD - Sends host capacity support information and activates
/// the card's initialization process
pub const ACMD41: u8 = 0x29;

//==============================================================================

/// status for card in the ready state
pub const R1_READY_STATE: u8 = 0x00;

/// status for card in the idle state
pub const R1_IDLE_STATE: u8 = 0x01;

/// status bit for illegal command
pub const R1_ILLEGAL_COMMAND: u8 = 0x04;

/// start data token for read or write single block*/
pub const DATA_START_BLOCK: u8 = 0xFE;

/// stop token for write multiple blocks*/
pub const STOP_TRAN_TOKEN: u8 = 0xFD;

/// start data token for write multiple blocks*/
pub const WRITE_MULTIPLE_TOKEN: u8 = 0xFC;

/// mask for data response tokens after a write block operation
pub const DATA_RES_MASK: u8 = 0x1F;

/// write data accepted token
pub const DATA_RES_ACCEPTED: u8 = 0x05;

/// Card Specific Data, version 1
#[derive(Default)]
pub struct CsdV1 {
    /// The 16-bytes of data in this Card Specific Data block
    pub data: [u8; 16],
}

/// Card Specific Data, version 2
#[derive(Default)]
pub struct CsdV2 {
    /// The 16-bytes of data in this Card Specific Data block
    pub data: [u8; 16],
}

/// Card Specific Data
pub enum Csd {
    /// A version 1 CSD
    V1(CsdV1),
    /// A version 2 CSD
    V2(CsdV2),
}

impl CsdV1 {
    /// Create a new, empty, CSD
    pub fn new() -> CsdV1 {
        CsdV1::default()
    }

    define_field!(csd_ver, u8, 0, 6, 2);
    define_field!(data_read_access_time1, u8, 1, 0, 8);
    define_field!(data_read_access_time2, u8, 2, 0, 8);
    define_field!(max_data_transfer_rate, u8, 3, 0, 8);
    define_field!(card_command_classes, u16, [(4, 0, 8), (5, 4, 4)]);
    define_field!(read_block_length, u8, 5, 0, 4);
    define_field!(read_partial_blocks, bool, 6, 7);
    define_field!(write_block_misalignment, bool, 6, 6);
    define_field!(read_block_misalignment, bool, 6, 5);
    define_field!(dsr_implemented, bool, 6, 4);
    define_field!(device_size, u32, [(6, 0, 2), (7, 0, 8), (8, 6, 2)]);
    define_field!(max_read_current_vdd_max, u8, 8, 0, 3);
    define_field!(max_read_current_vdd_min, u8, 8, 3, 3);
    define_field!(max_write_current_vdd_max, u8, 9, 2, 3);
    define_field!(max_write_current_vdd_min, u8, 9, 5, 3);
    define_field!(device_size_multiplier, u8, [(9, 0, 2), (10, 7, 1)]);
    define_field!(erase_single_block_enabled, bool, 10, 6);
    define_field!(erase_sector_size, u8, [(10, 0, 6), (11, 7, 1)]);
    define_field!(write_protect_group_size, u8, 11, 0, 7);
    define_field!(write_protect_group_enable, bool, 12, 7);
    define_field!(write_speed_factor, u8, 12, 2, 3);
    define_field!(max_write_data_length, u8, [(12, 0, 2), (13, 6, 2)]);
    define_field!(write_partial_blocks, bool, 13, 5);
    define_field!(file_format, u8, 14, 2, 2);
    define_field!(temporary_write_protection, bool, 14, 4);
    define_field!(permanent_write_protection, bool, 14, 5);
    define_field!(copy_flag_set, bool, 14, 6);
    define_field!(file_format_group_set, bool, 14, 7);
    define_field!(crc, u8, 15, 0, 8);

    /// Returns the card capacity in bytes
    pub fn card_capacity_bytes(&self) -> u64 {
        let multiplier = self.device_size_multiplier() + self.read_block_length() + 2;
        (u64::from(self.device_size()) + 1) << multiplier
    }

    /// Returns the card capacity in 512-byte blocks
    pub fn card_capacity_blocks(&self) -> u32 {
        let multiplier = self.device_size_multiplier() + self.read_block_length() - 7;
        (self.device_size() + 1) << multiplier
    }
}

impl CsdV2 {
    /// Create a new, empty, CSD
    pub fn new() -> CsdV2 {
        CsdV2::default()
    }

    define_field!(csd_ver, u8, 0, 6, 2);
    define_field!(data_read_access_time1, u8, 1, 0, 8);
    define_field!(data_read_access_time2, u8, 2, 0, 8);
    define_field!(max_data_transfer_rate, u8, 3, 0, 8);
    define_field!(card_command_classes, u16, [(4, 0, 8), (5, 4, 4)]);
    define_field!(read_block_length, u8, 5, 0, 4);
    define_field!(read_partial_blocks, bool, 6, 7);
    define_field!(write_block_misalignment, bool, 6, 6);
    define_field!(read_block_misalignment, bool, 6, 5);
    define_field!(dsr_implemented, bool, 6, 4);
    define_field!(device_size, u32, [(7, 0, 6), (8, 0, 8), (9, 0, 8)]);
    define_field!(erase_single_block_enabled, bool, 10, 6);
    define_field!(erase_sector_size, u8, [(10, 0, 6), (11, 7, 1)]);
    define_field!(write_protect_group_size, u8, 11, 0, 7);
    define_field!(write_protect_group_enable, bool, 12, 7);
    define_field!(write_speed_factor, u8, 12, 2, 3);
    define_field!(max_write_data_length, u8, [(12, 0, 2), (13, 6, 2)]);
    define_field!(write_partial_blocks, bool, 13, 5);
    define_field!(file_format, u8, 14, 2, 2);
    define_field!(temporary_write_protection, bool, 14, 4);
    define_field!(permanent_write_protection, bool, 14, 5);
    define_field!(copy_flag_set, bool, 14, 6);
    define_field!(file_format_group_set, bool, 14, 7);
    define_field!(crc, u8, 15, 0, 8);

    /// Returns the card capacity in bytes
    pub fn card_capacity_bytes(&self) -> u64 {
        (u64::from(self.device_size()) + 1) * 512 * 1024
    }

    /// Returns the card capacity in 512-byte blocks
    pub fn card_capacity_blocks(&self) -> u32 {
        (self.device_size() + 1) * 1024
    }
}

/// Perform the 7-bit CRC used on the SD card
pub fn crc7(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for mut d in data.iter().cloned() {
        for _bit in 0..8 {
            crc <<= 1;
            if ((d & 0x80) ^ (crc & 0x80)) != 0 {
                crc ^= 0x09;
            }
            d <<= 1;
        }
    }
    (crc << 1) | 1
}

/// Perform the X25 CRC calculation, as used for data blocks.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc = ((crc >> 8) & 0xFF) | (crc << 8);
        crc ^= u16::from(byte);
        crc ^= (crc & 0xFF) >> 4;
        crc ^= crc << 12;
        crc ^= (crc & 0xFF) << 5;
    }
    crc
}
