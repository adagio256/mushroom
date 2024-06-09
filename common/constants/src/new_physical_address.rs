use x86_64::{
    structures::paging::{frame::PhysFrameRange, PageSize, PhysFrame, Size1GiB, Size2MiB},
    PhysAddr,
};

const fn addr<S>(addr: u64) -> PhysFrame<S>
where
    S: PageSize,
{
    let addr = PhysAddr::new(addr);
    let Ok(frame) = PhysFrame::from_start_address(addr) else {
        unreachable!()
    };
    frame
}

const fn addr_range<S>(start: u64, end: u64) -> PhysFrameRange<S>
where
    S: PageSize,
{
    assert!(start < end);
    PhysFrame::range(addr(start), addr(end + 1))
}

pub mod kernel {
    use super::*;

    // The segments of the kernel binary:
    pub const RESET_VECTOR: PhysFrame<Size2MiB> = addr(0x10040000000);
    pub const TEXT: PhysFrameRange<Size2MiB> = addr_range(0x10040200000, 0x10040ffffff);
    pub const RODATA: PhysFrameRange<Size2MiB> = addr_range(0x10080000000, 0x10080ffffff);
    pub const DATA: PhysFrameRange<Size2MiB> = addr_range(0x100c0000000, 0x100c0ffffff);
    pub const TDATA: PhysFrameRange<Size2MiB> = addr_range(0x10100000000, 0x10100ffffff);
    pub const STACK: PhysFrameRange<Size2MiB> = addr_range(0x10140000000, 0x10140ffffff);

    // The shadow memory segments of the kernel binary (for KASAN):
    pub const TEXT_SHADOW: PhysFrame<Size2MiB> = addr(0x18000000000);
    pub const RODATA_SHADOW: PhysFrame<Size2MiB> = addr(0x18000200000);
    pub const DATA_SHADOW: PhysFrame<Size2MiB> = addr(0x18000400000);
    pub const TDATA_SHADOW: PhysFrame<Size2MiB> = addr(0x18000600000);
    pub const STACK_SHADOW: PhysFrame<Size2MiB> = addr(0x18000800000);
}

// 64 gibibytes of dynamic physical memory that can be hot-plugged and hot-unplugged.
pub const DYNAMIC: PhysFrameRange<Size1GiB> = addr_range(0x020000000000, 0x20fffffffff);

// Regions for kernel-guest communication during profiling.
pub const PROFILER_CONTROL: PhysFrameRange<Size2MiB> = addr_range(0x80000000000, 0x80000ffffff);
pub const PROFILER_BUFFER: PhysFrame<Size1GiB> = addr(0x80040000000);
