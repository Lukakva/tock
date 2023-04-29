/// Represents a hardware or software device that has data organized in
/// fixed block sizes. For example, external storage devices such as
/// SD cards, Flash drives, Hard Drives.
pub struct BlockDevice {
    /// The block size of this device. All read-write operations operate
    /// with chunks of this size.
    block_size: u32,
}
