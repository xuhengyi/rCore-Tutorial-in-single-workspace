#![no_std]
#![no_main]
// #![deny(warnings)]

mod process;

#[cfg(feature = "nobios")]
mod msbi;

#[macro_use]
extern crate rcore_console;

extern crate alloc;

use crate::{
    impls::SyscallContext,
    process::Process,
};
use alloc::{alloc::alloc, vec::Vec};
use core::alloc::Layout;
use impls::Console;
use kernel_context::{foreign::MultislotPortal, LocalContext};
use rcore_console::log;
use riscv::register::*;
use sbi_rt::*;
use syscall::Caller;
use xmas_elf::ElfFile;

// 根据架构选择页表模式
#[cfg(target_pointer_width = "64")]
use impls::Sv39Manager as VmManager;
#[cfg(target_pointer_width = "64")]
use kernel_vm::page_table::Sv39 as VmMode;

#[cfg(target_pointer_width = "32")]
use impls::Sv32Manager as VmManager;
#[cfg(target_pointer_width = "32")]
use kernel_vm::page_table::Sv32 as VmMode;

use kernel_vm::{
    page_table::{MmuMeta, VAddr, VmFlags, VmMeta, PPN, VPN},
    AddressSpace,
};

// 应用程序内联进来。
core::arch::global_asm!(include_str!(env!("APP_ASM")));

// M-Mode 入口汇编（仅在 nobios 模式下）
// 根据目标架构选择正确的汇编文件
#[cfg(all(feature = "nobios", target_pointer_width = "64"))]
core::arch::global_asm!(include_str!("m_entry_rv64.S"));

#[cfg(all(feature = "nobios", target_pointer_width = "32"))]
core::arch::global_asm!(include_str!("m_entry_rv32.S"));

// 定义内核入口。
linker::boot0!(rust_main; stack = 6 * 4096);
// 物理内存容量 = 24 MiB。
const MEMORY: usize = 24 << 20;
// 传送门所在虚页。
const PROTAL_TRANSIT: VPN<VmMode> = VPN::MAX;
// 进程列表。
static mut PROCESSES: Vec<Process> = Vec::new();

extern "C" fn rust_main() -> ! {
    let layout = linker::KernelLayout::locate();
    // bss 段清零
    unsafe { layout.zero_bss() };
    // 初始化 `console`
    rcore_console::init_console(&Console);
    rcore_console::set_timestamp(impls::monotonic_time_ms);
    rcore_console::set_log_level(option_env!("LOG"));
    rcore_console::test_log();
    // 初始化内核堆
    kernel_alloc::init(layout.start() as _);
    unsafe {
        kernel_alloc::transfer(core::slice::from_raw_parts_mut(
            layout.end() as _,
            MEMORY - layout.len(),
        ))
    };
    // 建立异界传送门
    let portal_size = MultislotPortal::calculate_size(1);
    let portal_layout = Layout::from_size_align(portal_size, 1 << VmMode::PAGE_BITS).unwrap();
    let portal_ptr = unsafe { alloc(portal_layout) };
    assert!(portal_layout.size() < 1 << VmMode::PAGE_BITS);
    // 建立内核地址空间
    let mut ks = kernel_space(layout, MEMORY, portal_ptr as _);
    let portal_idx = PROTAL_TRANSIT.index_in(VmMode::MAX_LEVEL);
    // 加载应用程序
    for (i, elf) in linker::AppMeta::locate().iter().enumerate() {
        let base = elf.as_ptr() as usize;
        log::info!("detect app[{i}]: {base:#x}..{:#x}", base + elf.len());
        if let Some(process) = Process::new(ElfFile::new(elf).unwrap()) {
            // 映射异界传送门
            process.address_space.root()[portal_idx] = ks.root()[portal_idx];
            unsafe { PROCESSES.push(process) };
        }
    }

    // 建立调度栈
    let page_layout: Layout =
        unsafe { Layout::from_size_align_unchecked(2 << VmMode::PAGE_BITS, 1 << VmMode::PAGE_BITS) };
    let pages = 2;
    let stack = unsafe { alloc(page_layout) };
    
    // RV64: 使用更大的地址空间
    #[cfg(target_pointer_width = "64")]
    let stack_top_vpn = 1usize << 26;
    // RV32: 使用较小的地址空间 (Sv32: 20-bit VPN)
    #[cfg(target_pointer_width = "32")]
    let stack_top_vpn = 1usize << 19;
    
    ks.map_extern(
        VPN::new(stack_top_vpn - pages)..VPN::new(stack_top_vpn),
        PPN::new(stack as usize >> VmMode::PAGE_BITS),
        VmFlags::build_from_str("_WRV"),
    );
    // 建立调度线程，目的是划分异常域。调度线程上发生内核异常时会回到这个控制流处理
    let mut scheduling = LocalContext::thread(schedule as _, false);
    
    // RV64: 栈顶在虚拟地址高位
    #[cfg(target_pointer_width = "64")]
    {
        *scheduling.sp_mut() = 1 << 38;
    }
    // RV32: 栈顶在较低的虚拟地址
    #[cfg(target_pointer_width = "32")]
    {
        *scheduling.sp_mut() = stack_top_vpn << VmMode::PAGE_BITS;
    }
    
    unsafe { scheduling.execute() };
    log::error!("stval = {:#x}", stval::read());
    panic!("trap from scheduling thread: {:?}", scause::read().cause());
}

extern "C" fn schedule() -> ! {
    // 初始化异界传送门
    let portal = unsafe { MultislotPortal::init_transit(PROTAL_TRANSIT.base().val(), 1) };
    // 初始化 syscall
    syscall::init_io(&SyscallContext);
    syscall::init_process(&SyscallContext);
    syscall::init_scheduling(&SyscallContext);
    syscall::init_clock(&SyscallContext);
    while !unsafe { PROCESSES.is_empty() } {
        let ctx = unsafe { &mut PROCESSES[0].context };
        unsafe { ctx.execute(portal, ()) };
        match scause::read().cause() {
            scause::Trap::Exception(scause::Exception::UserEnvCall) => {
                use syscall::{SyscallId as Id, SyscallResult as Ret};

                let ctx = &mut ctx.context;
                let id: Id = ctx.a(7).into();
                let args = [ctx.a(0), ctx.a(1), ctx.a(2), ctx.a(3), ctx.a(4), ctx.a(5)];
                match syscall::handle(Caller { entity: 0, flow: 0 }, id, args) {
                    Ret::Done(ret) => match id {
                        Id::EXIT => unsafe {
                            PROCESSES.remove(0);
                        },
                        _ => {
                            *ctx.a_mut(0) = ret as _;
                            ctx.move_next();
                        }
                    },
                    Ret::Unsupported(_) => {
                        log::info!("id = {id:?}");
                        unsafe { PROCESSES.remove(0) };
                    }
                }
            }
            e => {
                log::error!(
                    "unsupported trap: {e:?}, stval = {:#x}, sepc = {:#x}",
                    stval::read(),
                    ctx.context.pc()
                );
                unsafe { PROCESSES.remove(0) };
            }
        }
    }
    system_reset(Shutdown, NoReason);
    unreachable!()
}

/// Rust 异常处理函数，以异常方式关机。
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log::error!("{info}");
    system_reset(Shutdown, SystemFailure);
    loop {}
}

fn kernel_space(
    layout: linker::KernelLayout,
    memory: usize,
    portal: usize,
) -> AddressSpace<VmMode, VmManager> {
    let mut space = AddressSpace::<VmMode, VmManager>::new();
    for region in layout.iter() {
        log::info!("{region}");
        use linker::KernelRegionTitle::*;
        let flags = match region.title {
            Text => "X_RV",
            Rodata => "__RV",
            Data | Boot => "_WRV",
        };
        let s = VAddr::<VmMode>::new(region.range.start);
        let e = VAddr::<VmMode>::new(region.range.end);
        space.map_extern(
            s.floor()..e.ceil(),
            PPN::new(s.floor().val()),
            VmFlags::build_from_str(flags),
        )
    }
    log::info!(
        "(heap) ---> {:#10x}..{:#10x}",
        layout.end(),
        layout.start() + memory
    );
    let s = VAddr::<VmMode>::new(layout.end());
    let e = VAddr::<VmMode>::new(layout.start() + memory);
    space.map_extern(
        s.floor()..e.ceil(),
        PPN::new(s.floor().val()),
        VmFlags::build_from_str("_WRV"),
    );
    space.map_extern(
        PROTAL_TRANSIT..PROTAL_TRANSIT + 1,
        PPN::new(portal >> VmMode::PAGE_BITS),
        VmFlags::build_from_str("__G_XWRV"),
    );
    println!();
    
    // 根据架构设置 satp
    #[cfg(target_pointer_width = "64")]
    unsafe { satp::set(satp::Mode::Sv39, 0, space.root_ppn().val()) };
    #[cfg(target_pointer_width = "32")]
    unsafe { satp::set(satp::Mode::Sv32, 0, space.root_ppn().val()) };
    
    space
}

/// 各种接口库的实现。
mod impls {
    use crate::PROCESSES;
    use alloc::alloc::alloc_zeroed;
    use core::{
        alloc::Layout,
        ptr::NonNull,
        sync::atomic::{AtomicBool, Ordering},
    };
    use kernel_vm::PageManager;
    use riscv::register::time;
    use rcore_console::log;
    use syscall::*;

    // ============ RV64 Sv39 支持 ============
    #[cfg(target_pointer_width = "64")]
    use kernel_vm::page_table::{MmuMeta, Pte, Sv39, VAddr, VmFlags, PPN, VPN};

    #[cfg(target_pointer_width = "64")]
    #[repr(transparent)]
    pub struct Sv39Manager(NonNull<Pte<Sv39>>);

    #[cfg(target_pointer_width = "64")]
    impl Sv39Manager {
        const OWNED: VmFlags<Sv39> = unsafe { VmFlags::from_raw(1 << 8) };

        #[inline]
        fn page_alloc<T>(count: usize) -> *mut T {
            unsafe {
                alloc_zeroed(Layout::from_size_align_unchecked(
                    count << Sv39::PAGE_BITS,
                    1 << Sv39::PAGE_BITS,
                ))
            }
            .cast()
        }
    }

    #[cfg(target_pointer_width = "64")]
    impl PageManager<Sv39> for Sv39Manager {
        #[inline]
        fn new_root() -> Self {
            Self(NonNull::new(Self::page_alloc(1)).unwrap())
        }

        #[inline]
        fn root_ppn(&self) -> PPN<Sv39> {
            PPN::new(self.0.as_ptr() as usize >> Sv39::PAGE_BITS)
        }

        #[inline]
        fn root_ptr(&self) -> NonNull<Pte<Sv39>> {
            self.0
        }

        #[inline]
        fn p_to_v<T>(&self, ppn: PPN<Sv39>) -> NonNull<T> {
            unsafe { NonNull::new_unchecked(VPN::<Sv39>::new(ppn.val()).base().as_mut_ptr()) }
        }

        #[inline]
        fn v_to_p<T>(&self, ptr: NonNull<T>) -> PPN<Sv39> {
            PPN::new(VAddr::<Sv39>::new(ptr.as_ptr() as _).floor().val())
        }

        #[inline]
        fn check_owned(&self, pte: Pte<Sv39>) -> bool {
            pte.flags().contains(Self::OWNED)
        }

        #[inline]
        fn allocate(&mut self, len: usize, flags: &mut VmFlags<Sv39>) -> NonNull<u8> {
            *flags |= Self::OWNED;
            NonNull::new(Self::page_alloc(len)).unwrap()
        }

        fn deallocate(&mut self, _pte: Pte<Sv39>, _len: usize) -> usize {
            todo!()
        }

        fn drop_root(&mut self) {
            todo!()
        }
    }

    // ============ RV32 Sv32 支持 ============
    #[cfg(target_pointer_width = "32")]
    use kernel_vm::page_table::{MmuMeta, Pte, Sv32, VAddr, VmFlags, PPN, VPN};

    #[cfg(target_pointer_width = "32")]
    #[repr(transparent)]
    pub struct Sv32Manager(NonNull<Pte<Sv32>>);

    #[cfg(target_pointer_width = "32")]
    impl Sv32Manager {
        const OWNED: VmFlags<Sv32> = unsafe { VmFlags::from_raw(1 << 8) };

        #[inline]
        fn page_alloc<T>(count: usize) -> *mut T {
            unsafe {
                alloc_zeroed(Layout::from_size_align_unchecked(
                    count << Sv32::PAGE_BITS,
                    1 << Sv32::PAGE_BITS,
                ))
            }
            .cast()
        }
    }

    #[cfg(target_pointer_width = "32")]
    impl PageManager<Sv32> for Sv32Manager {
        #[inline]
        fn new_root() -> Self {
            Self(NonNull::new(Self::page_alloc(1)).unwrap())
        }

        #[inline]
        fn root_ppn(&self) -> PPN<Sv32> {
            PPN::new(self.0.as_ptr() as usize >> Sv32::PAGE_BITS)
        }

        #[inline]
        fn root_ptr(&self) -> NonNull<Pte<Sv32>> {
            self.0
        }

        #[inline]
        fn p_to_v<T>(&self, ppn: PPN<Sv32>) -> NonNull<T> {
            unsafe { NonNull::new_unchecked(VPN::<Sv32>::new(ppn.val()).base().as_mut_ptr()) }
        }

        #[inline]
        fn v_to_p<T>(&self, ptr: NonNull<T>) -> PPN<Sv32> {
            PPN::new(VAddr::<Sv32>::new(ptr.as_ptr() as _).floor().val())
        }

        #[inline]
        fn check_owned(&self, pte: Pte<Sv32>) -> bool {
            pte.flags().contains(Self::OWNED)
        }

        #[inline]
        fn allocate(&mut self, len: usize, flags: &mut VmFlags<Sv32>) -> NonNull<u8> {
            *flags |= Self::OWNED;
            NonNull::new(Self::page_alloc(len)).unwrap()
        }

        fn deallocate(&mut self, _pte: Pte<Sv32>, _len: usize) -> usize {
            todo!()
        }

        fn drop_root(&mut self) {
            todo!()
        }
    }

    pub struct Console;

    impl rcore_console::Console for Console {
        #[inline]
        fn put_char(&self, c: u8) {
            #[allow(deprecated)]
            sbi_rt::legacy::console_putchar(c as _);
        }
    }

    pub struct SyscallContext;

    // 使用 crate 级别的类型别名
    #[cfg(target_pointer_width = "64")]
    type VmModeLocal = Sv39;
    #[cfg(target_pointer_width = "32")]
    type VmModeLocal = Sv32;

    impl IO for SyscallContext {
        fn write(&self, caller: Caller, fd: usize, buf: usize, count: usize) -> isize {
            match fd {
                STDOUT | STDDEBUG => {
                    const READABLE: VmFlags<VmModeLocal> = VmFlags::build_from_str("RV");
                    if let Some(ptr) = unsafe { PROCESSES.get_mut(caller.entity) }
                        .unwrap()
                        .address_space
                        .translate(VAddr::new(buf), READABLE)
                    {
                        print_with_timestamp(unsafe {
                            core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                                ptr.as_ptr(),
                                count,
                            ))
                        });
                        count as _
                    } else {
                        log::error!("ptr not readable");
                        -1
                    }
                }
                _ => {
                    log::error!("unsupported fd: {fd}");
                    -1
                }
            }
        }
    }

    impl Process for SyscallContext {
        #[inline]
        fn exit(&self, _caller: Caller, _status: usize) -> isize {
            0
        }
    }

    impl Scheduling for SyscallContext {
        #[inline]
        fn sched_yield(&self, _caller: Caller) -> isize {
            0
        }
    }

    impl Clock for SyscallContext {
        #[inline]
        fn clock_gettime(&self, caller: Caller, clock_id: ClockId, tp: usize) -> isize {
            const WRITABLE: VmFlags<VmModeLocal> = VmFlags::build_from_str("W_V");
            match clock_id {
                ClockId::CLOCK_MONOTONIC => {
                    if let Some(mut ptr) = unsafe { PROCESSES.get(caller.entity) }
                        .unwrap()
                        .address_space
                        .translate(VAddr::new(tp), WRITABLE)
                    {
                        let time = monotonic_time_ns();
                        *unsafe { ptr.as_mut() } = TimeSpec {
                            tv_sec: time / 1_000_000_000,
                            tv_nsec: time % 1_000_000_000,
                        };
                        0
                    } else {
                        log::error!("ptr not readable");
                        -1
                    }
                }
                _ => -1,
            }
        }
    }

    static LINE_START: AtomicBool = AtomicBool::new(true);

    #[inline]
    pub(crate) fn monotonic_time_ms() -> usize {
        monotonic_time_ns() / 1_000_000
    }

    #[inline]
    fn monotonic_time_ns() -> usize {
        #[cfg(target_pointer_width = "64")]
        {
            (time::read64() as u64 * 10000 / 125) as usize
        }
        #[cfg(target_pointer_width = "32")]
        {
            // RV32: 使用 rdtime 读取低32位
            (time::read() as u64 * 10000 / 125) as usize
        }
    }

    fn print_with_timestamp(s: &str) {
        let mut at_line_start = LINE_START.load(Ordering::Relaxed);
        for segment in s.split_inclusive('\n') {
            if at_line_start {
                let ts_ms = monotonic_time_ms();
                print!("[{ts_ms:>5} ms] ");
            }
            print!("{segment}");
            at_line_start = segment.ends_with('\n');
        }
        LINE_START.store(at_line_start, Ordering::Relaxed);
    }
}
