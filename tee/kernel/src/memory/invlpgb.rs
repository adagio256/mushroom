use core::{arch::asm, iter::Step};

use bit_field::BitField;
use bitflags::bitflags;
use x86_64::{
    instructions::tlb::{Invlpgb, Pcid},
    registers::{control::Cr3, model_specific::Msr},
    structures::paging::Page,
};

use crate::spin::lazy::Lazy;

pub static INVLPGB: Lazy<InvlpgbCompat> = Lazy::new(InvlpgbCompat::new);

pub struct InvlpgbCompat {
    invlpgb: Option<Invlpgb>,
}

impl InvlpgbCompat {
    fn new() -> Self {
        Self {
            invlpgb: Invlpgb::new(),
        }
    }

    pub fn flush_pcid(&self, pcid: Pcid) {
        if let Some(invlpgb) = self.invlpgb.as_ref() {
            unsafe {
                invlpgb.build().pcid(pcid).flush();
            }

            invlpgb.tlbsync();
        } else {
            hv_flush_all();
        }
    }

    pub fn flush_page(&self, page: Page, global: bool) {
        if let Some(invlpgb) = self.invlpgb.as_ref() {
            let flush = invlpgb.build();
            let next_page = Step::forward(page, 1);
            let flush = flush.pages(Page::range(page, next_page));
            let flush = if global {
                flush.include_global()
            } else {
                flush
            };
            flush.flush();

            invlpgb.tlbsync();
        } else {
            hv_flush_all();
        }
    }
}
fn hv_flush_all() {
    unsafe {
        Msr::new(0x40000000).write(1);
    }

    let mut hypercall_input = 0;
    hypercall_input.set_bits(0..=15, 0x0002); // call code: HvCallFlushVirtualAddressSpace
    hypercall_input.set_bit(16, true); // fast
                                       // hypercall_input.set_bit(17..=26, 3); // size of header in qwords

    let flags = HvCallFlushVirtualAddressSpaceFlags::HV_FLUSH_ALL_PROCESSORS
        | HvCallFlushVirtualAddressSpaceFlags::HV_FLUSH_ALL_VIRTUAL_ADDRESS_SPACES;

    let result: u64;

    unsafe {
        asm! {
            "vpxor xmm0, xmm0, xmm0",
            "vmmcall",
            in("rcx") hypercall_input,
            in("rdx") flags.bits(),
            inout("rax") 0x5a5a5a5a5a5a5a5au64 => result,
        };
    }

    assert_eq!(result.get_bits(0..=15), 0);
}

bitflags! {
    #[repr(transparent)]
    pub struct HvCallFlushVirtualAddressSpaceFlags: u64 {
        const HV_FLUSH_ALL_PROCESSORS = 1 << 0;
        const HV_FLUSH_ALL_VIRTUAL_ADDRESS_SPACES = 1 << 1;
        const HV_FLUSH_NON_GLOBAL_MAPPINGS_ONLY = 1 << 2;
    }
}

struct HvCallFlushVirtualAddressSpace {
    address_space: u64,
    flags: HvCallFlushVirtualAddressSpaceFlags,
    processor_mask: u64,
}
