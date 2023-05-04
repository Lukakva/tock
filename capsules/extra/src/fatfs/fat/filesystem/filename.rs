/// An MS-DOS 8.3 filename. 7-bit ASCII only. All lower-case is converted to
/// upper-case by default.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct ShortFileName {
    pub(crate) contents: [u8; 11],
}

/// Various filename related errors that can occur.
#[cfg_attr(feature = "defmt-log", derive(defmt::Format))]
#[derive(Debug, Clone)]
pub enum FilenameError {
    /// Tried to create a file with an invalid character.
    InvalidCharacter,
    /// Tried to create a file with no file name.
    FilenameEmpty,
    /// Given name was too long (we are limited to 8.3).
    NameTooLong,
    /// Can't start a file with a period, or after 8 characters.
    MisplacedPeriod,
    /// Can't extract utf8 from file name
    Utf8Error,
}

impl FilenameError {}

impl ShortFileName {
    const FILENAME_BASE_MAX_LEN: usize = 8;
    const FILENAME_MAX_LEN: usize = 11;

    /// Get base name (name without extension) of file name
    pub fn base_name(&self) -> &[u8] {
        Self::bytes_before_space(&self.contents[..Self::FILENAME_BASE_MAX_LEN])
    }

    /// Get base name (name without extension) of file name
    pub fn extension(&self) -> &[u8] {
        Self::bytes_before_space(&self.contents[Self::FILENAME_BASE_MAX_LEN..])
    }

    fn bytes_before_space(bytes: &[u8]) -> &[u8] {
        bytes.split(|b| *b == b' ').next().unwrap_or(&bytes[0..0])
    }

    /// Create a new MS-DOS 8.3 space-padded file name as stored in the directory entry.
    pub fn create_from_buffer(name: &[u8]) -> Result<ShortFileName, FilenameError> {
        let mut sfn = ShortFileName {
            contents: [b' '; Self::FILENAME_MAX_LEN],
        };
        let mut idx = 0;
        let mut seen_dot = false;
        for ch in name.iter() {
            match ch {
                // Microsoft say these are the invalid characters
                0x00..=0x1F
                | 0x20
                | 0x22
                | 0x2A
                | 0x2B
                | 0x2C
                | 0x2F
                | 0x3A
                | 0x3B
                | 0x3C
                | 0x3D
                | 0x3E
                | 0x3F
                | 0x5B
                | 0x5C
                | 0x5D
                | 0x7C => {
                    return Err(FilenameError::InvalidCharacter);
                }
                // Denotes the start of the file extension
                b'.' => {
                    if (1..=Self::FILENAME_BASE_MAX_LEN).contains(&idx) {
                        idx = Self::FILENAME_BASE_MAX_LEN;
                        seen_dot = true;
                    } else {
                        return Err(FilenameError::MisplacedPeriod);
                    }
                }
                _ => {
                    let ch = if (b'a'..=b'z').contains(&ch) {
                        // Uppercase characters only
                        *ch - 32
                    } else {
                        *ch
                    };
                    if seen_dot {
                        if (Self::FILENAME_BASE_MAX_LEN..Self::FILENAME_MAX_LEN).contains(&idx) {
                            sfn.contents[idx] = ch;
                        } else {
                            return Err(FilenameError::NameTooLong);
                        }
                    } else if idx < Self::FILENAME_BASE_MAX_LEN {
                        sfn.contents[idx] = ch;
                    } else {
                        return Err(FilenameError::NameTooLong);
                    }
                    idx += 1;
                }
            }
        }
        if idx == 0 {
            return Err(FilenameError::FilenameEmpty);
        }
        Ok(sfn)
    }

    /// Create a new MS-DOS 8.3 space-padded file name as stored in the directory entry.
    /// Use this for volume labels with mixed case.
    pub fn create_from_buffer_mixed_case(name: &[u8]) -> Result<ShortFileName, FilenameError> {
        let mut sfn = ShortFileName {
            contents: [b' '; Self::FILENAME_MAX_LEN],
        };
        let mut idx = 0;
        let mut seen_dot = false;
        for ch in name.iter() {
            match ch {
                // Microsoft say these are the invalid characters
                0x00..=0x1F
                | 0x20
                | 0x22
                | 0x2A
                | 0x2B
                | 0x2C
                | 0x2F
                | 0x3A
                | 0x3B
                | 0x3C
                | 0x3D
                | 0x3E
                | 0x3F
                | 0x5B
                | 0x5C
                | 0x5D
                | 0x7C => {
                    return Err(FilenameError::InvalidCharacter);
                }
                // Denotes the start of the file extension
                b'.' => {
                    if (1..=Self::FILENAME_BASE_MAX_LEN).contains(&idx) {
                        idx = Self::FILENAME_BASE_MAX_LEN;
                        seen_dot = true;
                    } else {
                        return Err(FilenameError::MisplacedPeriod);
                    }
                }
                _ => {
                    if seen_dot {
                        if (Self::FILENAME_BASE_MAX_LEN..Self::FILENAME_MAX_LEN).contains(&idx) {
                            sfn.contents[idx] = *ch;
                        } else {
                            return Err(FilenameError::NameTooLong);
                        }
                    } else if idx < Self::FILENAME_BASE_MAX_LEN {
                        sfn.contents[idx] = *ch;
                    } else {
                        return Err(FilenameError::NameTooLong);
                    }
                    idx += 1;
                }
            }
        }
        if idx == 0 {
            return Err(FilenameError::FilenameEmpty);
        }
        Ok(sfn)
    }
}

impl core::fmt::Display for ShortFileName {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut printed = 0;
        for (i, &c) in self.contents.iter().enumerate() {
            if c != b' ' {
                if i == Self::FILENAME_BASE_MAX_LEN {
                    write!(f, ".")?;
                    printed += 1;
                }
                write!(f, "{}", c as char)?;
                printed += 1;
            }
        }
        if let Some(mut width) = f.width() {
            if width > printed {
                width -= printed;
                for _ in 0..width {
                    write!(f, "{}", f.fill())?;
                }
            }
        }
        Ok(())
    }
}

impl core::fmt::Debug for ShortFileName {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "ShortFileName(\"{}\")", self)
    }
}
