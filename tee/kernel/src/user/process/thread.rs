use core::{
    arch::asm,
    ffi::CStr,
    ops::{BitAndAssign, BitOrAssign, Deref, DerefMut, Not},
    sync::atomic::{AtomicU32, Ordering},
};

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use bitflags::bitflags;
use bytemuck::{offset_of, Pod, Zeroable};
use futures::{select_biased, FutureExt};
use spin::{Mutex, MutexGuard};
use x86_64::{
    registers::segmentation::{Segment64, FS},
    VirtAddr,
};

use crate::{
    error::{Error, Result},
    fs::{
        node::{lookup_and_resolve_node, File, FileSnapshot, ROOT_NODE},
        path::Path,
    },
    per_cpu::{PerCpu, KERNEL_REGISTERS_OFFSET, USERSPACE_REGISTERS_OFFSET},
    rt::{mpmc, once::OnceCell, oneshot, spawn},
};

use super::{
    fd::FileDescriptorTable,
    memory::{VirtualMemory, VirtualMemoryActivator},
    syscall::args::FileMode,
    Process,
};

pub static THREADS: Threads = Threads::new();

pub fn new_tid() -> u32 {
    static PID_COUNTER: AtomicU32 = AtomicU32::new(1);
    PID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub struct Threads {
    map: Mutex<BTreeMap<u32, Arc<Thread>>>,
}

impl Threads {
    const fn new() -> Self {
        Self {
            map: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn add(&self, thread: Arc<Thread>) -> u32 {
        let tid = thread.tid;
        self.map.lock().insert(tid, thread);
        tid
    }

    pub fn by_id(&self, tid: u32) -> Option<Arc<Thread>> {
        self.map.lock().get(&tid).cloned()
    }

    pub fn remove(&self, tid: u32) {
        self.map.lock().remove(&tid);
    }
}

pub type WeakThread = Weak<Thread>;

pub struct Thread {
    // Immutable state.
    tid: u32,
    self_weak: WeakThread,
    parent: WeakThread,
    process: Arc<Process>,
    fdtable: Arc<FileDescriptorTable>,
    dead_children: mpmc::Receiver<(u32, u8)>,
    exit_status: OnceCell<u8>,

    // Mutable state.
    state: Mutex<ThreadState>,
}

pub struct ThreadState {
    virtual_memory: Arc<VirtualMemory>,

    pub registers: UserspaceRegisters,

    pub sigmask: Sigset,
    pub sigaction: [Sigaction; 64],
    pub sigaltstack: Option<Stack>,
    pub clear_child_tid: u64,
    pub cwd: Path,
    pub vfork_done: Option<oneshot::Sender<()>>,
}

impl Thread {
    #[allow(clippy::too_many_arguments)]
    fn new(
        tid: u32,
        self_weak: WeakThread,
        parent: WeakThread,
        process: Arc<Process>,
        virtual_memory: Arc<VirtualMemory>,
        fdtable: Arc<FileDescriptorTable>,
        cwd: Path,
        vfork_done: Option<oneshot::Sender<()>>,
    ) -> Self {
        let registers = UserspaceRegisters::DEFAULT;
        Self {
            tid,
            self_weak,
            parent,
            process,
            fdtable,
            dead_children: mpmc::Receiver::new(),
            exit_status: OnceCell::new(),
            state: Mutex::new(ThreadState {
                virtual_memory,
                registers,
                sigmask: Sigset(0),
                sigaction: [Sigaction::DEFAULT; 64],
                sigaltstack: None,
                clear_child_tid: 0,
                cwd,
                vfork_done,
            }),
        }
    }

    pub fn spawn(create_thread: impl FnOnce(WeakThread) -> Result<Thread>) -> Result<u32> {
        let mut error = None;

        // Create the thread.
        let arc = Arc::new_cyclic(|self_weak| {
            let res = create_thread(self_weak.clone());
            res.unwrap_or_else(|err| {
                error = Some(err);

                // Create a temporary fake thread.
                // FIXME: Why isn't try_new_cyclic a thing?
                Thread::empty(self_weak.clone(), new_tid())
            })
        });

        if let Some(error) = error {
            return Err(error);
        }

        // Register the thread.
        let tid = THREADS.add(arc.clone());

        spawn(arc.run());

        Ok(tid)
    }

    pub fn empty(self_weak: WeakThread, tid: u32) -> Self {
        Self::new(
            tid,
            self_weak,
            Weak::new(),
            Arc::new(Process::new(tid)),
            Arc::new(VirtualMemory::new()),
            Arc::new(FileDescriptorTable::new()),
            Path::new(b"/".to_vec()).unwrap(),
            None,
        )
    }

    pub fn lock(&self) -> ThreadGuard {
        ThreadGuard {
            thread: self,
            state: self.state.lock(),
        }
    }

    pub fn tid(&self) -> u32 {
        self.tid
    }

    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    pub async fn run(self: Arc<Self>) {
        let thread_exit_future = self.exit_status.get();
        let process_exit_future = self.process.exit_status();
        let run_future = async {
            loop {
                let clone = self.clone();
                VirtualMemoryActivator::r#do(move |vm_activator| {
                    let mut guard = clone.lock();
                    guard.run_userspace(vm_activator);
                })
                .await;

                self.clone().execute_syscall().await;
            }
        };

        select_biased! {
            _ = thread_exit_future.fuse() => {}
            status = process_exit_future.fuse() => self.clone().exit(status).await,
            _ = run_future.fuse() => unreachable!(),
        }
    }

    async fn exit(self: Arc<Self>, status: u8) {
        VirtualMemoryActivator::r#do(move |vm_activator| {
            let mut guard = self.lock();
            guard.exit(vm_activator, status)
        })
        .await
    }

    pub async fn wait_for_exit(&self) -> u8 {
        *self.exit_status.get().await
    }

    pub fn try_wait_for_child_death(&self) -> Option<(u32, u8)> {
        self.dead_children.try_recv()
    }

    pub async fn wait_for_child_death(&self) -> (u32, u8) {
        self.dead_children.recv().await
    }

    pub fn add_child_death(&self, tid: u32, status: u8) {
        let _ = self.dead_children.sender().send((tid, status));
    }
}

pub struct ThreadGuard<'a> {
    thread: &'a Thread,
    state: MutexGuard<'a, ThreadState>,
}

impl ThreadGuard<'_> {
    #[allow(clippy::too_many_arguments)]
    pub fn clone(
        &self,
        new_tid: u32,
        self_weak: WeakThread,
        new_process: Option<Arc<Process>>,
        new_virtual_memory: Option<Arc<VirtualMemory>>,
        fdtable: Arc<FileDescriptorTable>,
        stack: VirtAddr,
        new_clear_child_tid: Option<VirtAddr>,
        new_tls: Option<u64>,
        vfork_done: Option<oneshot::Sender<()>>,
    ) -> Thread {
        let process = new_process.unwrap_or_else(|| self.process().clone());
        let virtual_memory = new_virtual_memory.unwrap_or_else(|| self.virtual_memory().clone());

        let thread = Thread::new(
            new_tid,
            self_weak,
            self.weak().clone(),
            process,
            virtual_memory,
            fdtable,
            self.cwd.clone(),
            vfork_done,
        );

        let mut guard = thread.lock();

        if let Some(clear_child_tid) = new_clear_child_tid {
            guard.clear_child_tid = clear_child_tid.as_u64();
        }

        // Copy all registers.
        guard.registers = self.registers;

        // Set the return value to 0 for the new thread.
        guard.registers.rax = 0;

        // Switch to a new stack if one is provided.
        if !stack.is_null() {
            guard.registers.rsp = stack.as_u64();
        }

        if let Some(tls) = new_tls {
            guard.registers.fs_base = tls;
        }

        drop(guard);

        thread
    }

    pub fn weak(&self) -> &WeakThread {
        &self.thread.self_weak
    }

    pub fn parent(&self) -> &WeakThread {
        &self.thread.parent
    }

    pub fn tid(&self) -> u32 {
        self.thread.tid
    }

    pub fn process(&self) -> &Arc<Process> {
        &self.thread.process
    }

    pub fn virtual_memory(&self) -> &Arc<VirtualMemory> {
        &self.virtual_memory
    }

    pub fn fdtable(&self) -> &Arc<FileDescriptorTable> {
        &self.thread.fdtable
    }

    pub fn execve(
        &mut self,
        path: &Path,
        argv: &[impl AsRef<CStr>],
        envp: &[impl AsRef<CStr>],
        vm_activator: &mut VirtualMemoryActivator,
    ) -> Result<()> {
        let node = lookup_and_resolve_node(ROOT_NODE.clone(), path)?;
        let file: Arc<dyn File> = node.try_into()?;
        if !file.mode().contains(FileMode::EXECUTE) {
            return Err(Error::acces(()));
        }
        let bytes = file.read_snapshot()?;

        self.start_executable(bytes, argv, envp, vm_activator)
    }

    pub fn start_executable(
        &mut self,
        bytes: FileSnapshot,
        argv: &[impl AsRef<CStr>],
        envp: &[impl AsRef<CStr>],
        vm_activator: &mut VirtualMemoryActivator,
    ) -> Result<()> {
        let virtual_memory = VirtualMemory::new();
        // Create stack.
        let len = 0x2_0000;
        let stack =
            vm_activator.activate(&virtual_memory, |vm| vm.allocate_stack(None, len))? + len;
        // Load the elf.
        let entry = vm_activator.activate(&virtual_memory, |vm| {
            vm.start_executable(bytes, stack, argv, envp)
        })?;

        // Success! Commit the new state to the thread.

        self.virtual_memory = Arc::new(virtual_memory);
        self.registers = UserspaceRegisters::DEFAULT;
        self.registers.rsp = stack.as_u64();
        self.registers.rip = entry;

        Ok(())
    }

    fn run_userspace(&mut self, vm_activator: &mut VirtualMemoryActivator) {
        let actual_kernel_registers_offset = offset_of!(PerCpu::new(), PerCpu, kernel_registers);
        assert_eq!(
            actual_kernel_registers_offset, KERNEL_REGISTERS_OFFSET,
            "the USERSPACE_REGISTERS_OFFSET needs to be adjusted to {actual_kernel_registers_offset}"
        );
        let actual_userspace_registers = offset_of!(PerCpu::new(), PerCpu, userspace_registers);
        assert_eq!(
            actual_userspace_registers, USERSPACE_REGISTERS_OFFSET,
            "the USERSPACE_REGISTERS_OFFSET needs to be adjusted to {actual_userspace_registers}"
        );

        macro_rules! kernel_reg_offset {
            ($ident:ident) => {{
                let registers = KernelRegisters::ZERO;
                let reference = &registers;
                let register = &registers.$ident;

                let reference = reference as *const KernelRegisters;
                let register = register as *const u64;

                let offset = unsafe { register.byte_offset_from(reference) };
                KERNEL_REGISTERS_OFFSET + (offset as usize)
            }};
        }
        macro_rules! userspace_reg_offset {
            ($ident:ident) => {{
                let registers = UserspaceRegisters::DEFAULT;
                let reference = &registers;
                let register = &registers.$ident;

                let reference = reference as *const UserspaceRegisters;
                let register = register as *const _ as *const ();

                let offset = unsafe { register.byte_offset_from(reference) };
                USERSPACE_REGISTERS_OFFSET + (offset as usize)
            }};
        }

        let per_cpu = PerCpu::get();
        per_cpu.userspace_registers.set(self.registers);
        per_cpu
            .current_virtual_memory
            .set(Some(self.virtual_memory().clone()));

        unsafe {
            FS::write_base(VirtAddr::new(self.registers.fs_base));
        }

        vm_activator.activate(self.virtual_memory(), |_| {
            unsafe {
                asm!(
                    // Set LSTAR
                    "lea rax, [rip+66f]",
                    "mov rdx, rax",
                    "shr rdx, 32",
                    "mov ecx, 0xC0000082",
                    "wrmsr",
                    // Save the kernel registers.
                    "mov gs:[{K_RAX_OFFSET}], rax",
                    "mov gs:[{K_RBX_OFFSET}], rbx",
                    "mov gs:[{K_RCX_OFFSET}], rcx",
                    "mov gs:[{K_RDX_OFFSET}], rdx",
                    "mov gs:[{K_RSI_OFFSET}], rsi",
                    "mov gs:[{K_RDI_OFFSET}], rdi",
                    "mov gs:[{K_RSP_OFFSET}], rsp",
                    "mov gs:[{K_RBP_OFFSET}], rbp",
                    "mov gs:[{K_R8_OFFSET}], r8",
                    "mov gs:[{K_R9_OFFSET}], r9",
                    "mov gs:[{K_R10_OFFSET}], r10",
                    "mov gs:[{K_R11_OFFSET}], r11",
                    "mov gs:[{K_R12_OFFSET}], r12",
                    "mov gs:[{K_R13_OFFSET}], r13",
                    "mov gs:[{K_R14_OFFSET}], r14",
                    "mov gs:[{K_R15_OFFSET}], r15",
                    // Save RFLAGS.
                    "pushfq",
                    "pop rax",
                    "mov gs:[{K_RFLAGS_OFFSET}], rax",
                    // Restore userspace registers.
                    "mov rax, gs:[{U_RAX_OFFSET}]",
                    "mov rbx, gs:[{U_RBX_OFFSET}]",
                    "mov rdx, gs:[{U_RDX_OFFSET}]",
                    "mov rsi, gs:[{U_RSI_OFFSET}]",
                    "mov rdi, gs:[{U_RDI_OFFSET}]",
                    "mov rsp, gs:[{U_RSP_OFFSET}]",
                    "mov rbp, gs:[{U_RBP_OFFSET}]",
                    "mov r8, gs:[{U_R8_OFFSET}]",
                    "mov r9, gs:[{U_R9_OFFSET}]",
                    "mov r10, gs:[{U_R10_OFFSET}]",
                    "mov r12, gs:[{U_R12_OFFSET}]",
                    "mov r13, gs:[{U_R13_OFFSET}]",
                    "mov r14, gs:[{U_R14_OFFSET}]",
                    "mov r15, gs:[{U_R15_OFFSET}]",
                    "mov rcx, gs:[{U_RIP_OFFSET}]",
                    "mov r11, gs:[{U_RFLAGS_OFFSET}]",
                    "vmovdqa ymm0, gs:[{U_YMM_OFFSET}+32*0]",
                    "vmovdqa ymm1, gs:[{U_YMM_OFFSET}+32*1]",
                    "vmovdqa ymm2, gs:[{U_YMM_OFFSET}+32*2]",
                    "vmovdqa ymm3, gs:[{U_YMM_OFFSET}+32*3]",
                    "vmovdqa ymm4, gs:[{U_YMM_OFFSET}+32*4]",
                    "vmovdqa ymm5, gs:[{U_YMM_OFFSET}+32*5]",
                    "vmovdqa ymm6, gs:[{U_YMM_OFFSET}+32*6]",
                    "vmovdqa ymm7, gs:[{U_YMM_OFFSET}+32*7]",
                    "vmovdqa ymm8, gs:[{U_YMM_OFFSET}+32*8]",
                    "vmovdqa ymm9, gs:[{U_YMM_OFFSET}+32*9]",
                    "vmovdqa ymm10, gs:[{U_YMM_OFFSET}+32*10]",
                    "vmovdqa ymm11, gs:[{U_YMM_OFFSET}+32*11]",
                    "vmovdqa ymm12, gs:[{U_YMM_OFFSET}+32*12]",
                    "vmovdqa ymm13, gs:[{U_YMM_OFFSET}+32*13]",
                    "vmovdqa ymm14, gs:[{U_YMM_OFFSET}+32*14]",
                    "vmovdqa ymm15, gs:[{U_YMM_OFFSET}+32*15]",
                    "ldmxcsr gs:[{U_MXCSR_OFFSET}]",
                    // Swap in userspace GS.
                    "swapgs",
                    // Enter usermdoe
                    "sysretq",
                    "66:",
                    // Swap in kernel GS.
                    "swapgs",
                    // Save userspace registers.
                    "mov gs:[{U_RAX_OFFSET}], rax",
                    "mov gs:[{U_RBX_OFFSET}], rbx",
                    "mov gs:[{U_RDX_OFFSET}], rdx",
                    "mov gs:[{U_RSI_OFFSET}], rsi",
                    "mov gs:[{U_RDI_OFFSET}], rdi",
                    "mov gs:[{U_RSP_OFFSET}], rsp",
                    "mov gs:[{U_RBP_OFFSET}], rbp",
                    "mov gs:[{U_R8_OFFSET}], r8",
                    "mov gs:[{U_R9_OFFSET}], r9",
                    "mov gs:[{U_R10_OFFSET}], r10",
                    "mov gs:[{U_R12_OFFSET}], r12",
                    "mov gs:[{U_R13_OFFSET}], r13",
                    "mov gs:[{U_R14_OFFSET}], r14",
                    "mov gs:[{U_R15_OFFSET}], r15",
                    "mov gs:[{U_RIP_OFFSET}], rcx",
                    "mov gs:[{U_RFLAGS_OFFSET}], r11",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*0], ymm0",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*1], ymm1",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*2], ymm2",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*3], ymm3",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*4], ymm4",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*5], ymm5",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*6], ymm6",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*7], ymm7",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*8], ymm8",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*9], ymm9",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*10], ymm10",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*11], ymm11",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*12], ymm12",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*13], ymm13",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*14], ymm14",
                    "vmovdqa gs:[{U_YMM_OFFSET}+32*15], ymm15",
                    "stmxcsr gs:[{U_MXCSR_OFFSET}]",
                    // Restore the kernel registers.
                    "mov rax, gs:[{K_RAX_OFFSET}]",
                    "mov rbx, gs:[{K_RBX_OFFSET}]",
                    "mov rcx, gs:[{K_RCX_OFFSET}]",
                    "mov rdx, gs:[{K_RDX_OFFSET}]",
                    "mov rsi, gs:[{K_RSI_OFFSET}]",
                    "mov rdi, gs:[{K_RDI_OFFSET}]",
                    "mov rsp, gs:[{K_RSP_OFFSET}]",
                    "mov rbp, gs:[{K_RBP_OFFSET}]",
                    "mov r8, gs:[{K_R8_OFFSET}]",
                    "mov r9, gs:[{K_R9_OFFSET}]",
                    "mov r10, gs:[{K_R10_OFFSET}]",
                    "mov r11, gs:[{K_R11_OFFSET}]",
                    "mov r12, gs:[{K_R12_OFFSET}]",
                    "mov r13, gs:[{K_R13_OFFSET}]",
                    "mov r14, gs:[{K_R14_OFFSET}]",
                    "mov r15, gs:[{K_R15_OFFSET}]",
                    // Restore RFLAGS.
                    "mov rax, gs:[{K_RFLAGS_OFFSET}]",
                    "push rax",
                    "popfq",
                    K_RAX_OFFSET = const kernel_reg_offset!(rax),
                    K_RBX_OFFSET = const kernel_reg_offset!(rbx),
                    K_RCX_OFFSET = const kernel_reg_offset!(rcx),
                    K_RDX_OFFSET = const kernel_reg_offset!(rdx),
                    K_RSI_OFFSET = const kernel_reg_offset!(rsi),
                    K_RDI_OFFSET = const kernel_reg_offset!(rdi),
                    K_RSP_OFFSET = const kernel_reg_offset!(rsp),
                    K_RBP_OFFSET = const kernel_reg_offset!(rbp),
                    K_R8_OFFSET = const kernel_reg_offset!(r8),
                    K_R9_OFFSET = const kernel_reg_offset!(r9),
                    K_R10_OFFSET = const kernel_reg_offset!(r10),
                    K_R11_OFFSET = const kernel_reg_offset!(r11),
                    K_R12_OFFSET = const kernel_reg_offset!(r12),
                    K_R13_OFFSET = const kernel_reg_offset!(r13),
                    K_R14_OFFSET = const kernel_reg_offset!(r14),
                    K_R15_OFFSET = const kernel_reg_offset!(r15),
                    K_RFLAGS_OFFSET = const kernel_reg_offset!(rflags),
                    U_RAX_OFFSET = const userspace_reg_offset!(rax),
                    U_RBX_OFFSET = const userspace_reg_offset!(rbx),
                    U_RDX_OFFSET = const userspace_reg_offset!(rdx),
                    U_RSI_OFFSET = const userspace_reg_offset!(rsi),
                    U_RDI_OFFSET = const userspace_reg_offset!(rdi),
                    U_RSP_OFFSET = const userspace_reg_offset!(rsp),
                    U_RBP_OFFSET = const userspace_reg_offset!(rbp),
                    U_R8_OFFSET = const userspace_reg_offset!(r8),
                    U_R9_OFFSET = const userspace_reg_offset!(r9),
                    U_R10_OFFSET = const userspace_reg_offset!(r10),
                    U_R12_OFFSET = const userspace_reg_offset!(r12),
                    U_R13_OFFSET = const userspace_reg_offset!(r13),
                    U_R14_OFFSET = const userspace_reg_offset!(r14),
                    U_R15_OFFSET = const userspace_reg_offset!(r15),
                    U_RIP_OFFSET = const userspace_reg_offset!(rip),
                    U_RFLAGS_OFFSET = const userspace_reg_offset!(rflags),
                    U_YMM_OFFSET = const userspace_reg_offset!(ymm),
                    U_MXCSR_OFFSET = const userspace_reg_offset!(mxcsr),
                    out("rax") _,
                    out("rdx") _,
                    out("rcx") _,
                    options(preserves_flags)
                );
            }
        });

        self.registers = per_cpu.userspace_registers.get();
    }

    pub fn set_exit_status(&self, status: u8) {
        self.thread.exit_status.set(status);
    }
}

impl Deref for ThreadGuard<'_> {
    type Target = ThreadState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for ThreadGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

#[derive(Clone, Copy)]
pub struct KernelRegisters {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    rsp: u64,
    rbp: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rflags: u64,
}

impl KernelRegisters {
    pub const ZERO: Self = Self {
        rax: 0,
        rbx: 0,
        rcx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rsp: 0,
        rbp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rflags: 0,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct UserspaceRegisters {
    pub rax: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    pub fs_base: u64,
    ymm: [Ymm; 16],
    mxcsr: u64,
}

impl UserspaceRegisters {
    pub const DEFAULT: Self = Self {
        rax: 0,
        rbx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rsp: 0,
        rbp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rip: 0,
        rflags: 0,
        fs_base: 0,
        ymm: [Ymm::ZERO; 16],
        mxcsr: 0x1f80,
    };
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(32))]
struct Ymm([u8; 32]);

impl Ymm {
    const ZERO: Self = Self([0; 32]);
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Sigaction {
    sa_handler_or_sigaction: u64,
    sa_mask: Sigset,
    flags: u64,
    sa_restorer: u64,
}

impl Sigaction {
    const DEFAULT: Self = Self {
        sa_handler_or_sigaction: 0,
        sa_mask: Sigset(0),
        flags: 0,
        sa_restorer: 0,
    };
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Sigset(u64);

impl BitOrAssign for Sigset {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAndAssign for Sigset {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Not for Sigset {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Stack {
    pub ss_sp: u64,
    pub flags: StackFlags,
    _pad: u32,
    pub size: u64,
}

bitflags! {
    #[derive(Pod, Zeroable)]
    #[repr(transparent)]
    pub struct StackFlags: i32 {
        const ONSTACK = 1 << 0;
        const DISABLE = 1 << 1;
        const AUTODISARM = 1 << 31;
    }
}
