use core::cell::LazyCell;

use constants::{FINISH_OUTPUT_MSR, HALT_PORT, MEMORY_MSR, SCHEDULE_PORT, UPDATE_OUTPUT_MSR};
use spin::Mutex;
use x86_64::{
    instructions::port::PortWriteOnly,
    registers::model_specific::Msr,
    structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size2MiB},
    PhysAddr,
};

use crate::memory::{frame::FRAME_ALLOCATOR, temporary::copy_into_frame};

pub struct Allocator;

unsafe impl FrameAllocator<Size2MiB> for Allocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size2MiB>> {
        let memory_msr = Msr::new(MEMORY_MSR);
        let addr = unsafe { memory_msr.read() };

        if addr == 0 {
            return None;
        }

        let addr = PhysAddr::new(addr);
        let frame = PhysFrame::from_start_address(addr).unwrap();
        Some(frame)
    }
}

impl FrameDeallocator<Size2MiB> for Allocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size2MiB>) {
        let addr = frame.start_address().as_u64();

        let mut memory_msr = Msr::new(MEMORY_MSR);
        unsafe {
            memory_msr.write(addr);
        }
    }
}

/// Halt this vcpu.
pub fn halt() {
    unsafe {
        PortWriteOnly::new(HALT_PORT).write(0u32);
    }
}

/// Tell the supervisor to schedule another vcpu.
pub fn schedule_vcpu() {
    unsafe {
        PortWriteOnly::new(SCHEDULE_PORT).write(1u32);
    }
}

pub fn output(bytes: &[u8]) {
    static FRAME: Mutex<LazyCell<PhysFrame>> = Mutex::new(LazyCell::new(|| {
        (&FRAME_ALLOCATOR)
            .allocate_frame()
            .expect("failed to allocate frame for output")
    }));

    let guard = FRAME.lock();
    let frame = **guard;

    for chunk in bytes.chunks(0x1000) {
        let mut buffer = [0; 0x1000];
        buffer[..chunk.len()].copy_from_slice(chunk);

        unsafe {
            copy_into_frame(frame, &buffer).expect("failed to copy into output frame");
        }

        let command = frame.start_address().as_u64() | (chunk.len() as u64 - 1);
        unsafe {
            Msr::new(UPDATE_OUTPUT_MSR).write(command);
        }
    }
}

/// Tell to supervisor to commit the output and produce and attestation report.
pub fn commit_output() -> ! {
    unsafe {
        Msr::new(FINISH_OUTPUT_MSR).write(1);
    }
    unreachable!();
}

/// Tell the supervisor that something went wrong and to discard the output.
pub fn fail() -> ! {
    unsafe {
        Msr::new(FINISH_OUTPUT_MSR).write(0);
    }
    unreachable!();
}
