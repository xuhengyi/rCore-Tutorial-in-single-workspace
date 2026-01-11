use crate::VmManager;
use alloc::alloc::alloc_zeroed;
use core::{alloc::Layout, str::FromStr};
use kernel_context::{foreign::ForeignContext, LocalContext};
use kernel_vm::{
    page_table::{MmuMeta, VAddr, VmFlags, PPN, VPN},
    AddressSpace,
};
use rcore_console::log;
use xmas_elf::{
    header::{self, HeaderPt2, Machine},
    program, ElfFile,
};

// 根据架构选择页表模式
#[cfg(target_pointer_width = "64")]
use kernel_vm::page_table::Sv39 as VmMode;
#[cfg(target_pointer_width = "32")]
use kernel_vm::page_table::Sv32 as VmMode;

/// 进程。
pub struct Process {
    pub context: ForeignContext,
    pub address_space: AddressSpace<VmMode, VmManager>,
}

impl Process {
    pub fn new(elf: ElfFile) -> Option<Self> {
        // 根据架构检查 ELF 头
        #[cfg(target_pointer_width = "64")]
        let entry = match elf.header.pt2 {
            HeaderPt2::Header64(pt2)
                if pt2.type_.as_type() == header::Type::Executable
                    && pt2.machine.as_machine() == Machine::RISC_V =>
            {
                pt2.entry_point as usize
            }
            _ => None?,
        };
        
        #[cfg(target_pointer_width = "32")]
        let entry = match elf.header.pt2 {
            HeaderPt2::Header32(pt2)
                if pt2.type_.as_type() == header::Type::Executable
                    && pt2.machine.as_machine() == Machine::RISC_V =>
            {
                pt2.entry_point as usize
            }
            _ => None?,
        };

        const PAGE_SIZE: usize = 1 << VmMode::PAGE_BITS;
        const PAGE_MASK: usize = PAGE_SIZE - 1;

        let mut address_space = AddressSpace::new();
        for program in elf.program_iter() {
            if !matches!(program.get_type(), Ok(program::Type::Load)) {
                continue;
            }

            let off_file = program.offset() as usize;
            let len_file = program.file_size() as usize;
            let off_mem = program.virtual_addr() as usize;
            let end_mem = off_mem + program.mem_size() as usize;
            assert_eq!(off_file & PAGE_MASK, off_mem & PAGE_MASK);

            let mut flags: [u8; 5] = *b"U___V";
            if program.flags().is_execute() {
                flags[1] = b'X';
            }
            if program.flags().is_write() {
                flags[2] = b'W';
            }
            if program.flags().is_read() {
                flags[3] = b'R';
            }
            address_space.map(
                VAddr::new(off_mem).floor()..VAddr::new(end_mem).ceil(),
                &elf.input[off_file..][..len_file],
                off_mem & PAGE_MASK,
                VmFlags::from_str(unsafe { core::str::from_utf8_unchecked(&flags) }).unwrap(),
            );
        }
        
        let stack = unsafe {
            alloc_zeroed(Layout::from_size_align_unchecked(
                2 << VmMode::PAGE_BITS,
                1 << VmMode::PAGE_BITS,
            ))
        };
        
        // RV64: 使用更大的地址空间
        #[cfg(target_pointer_width = "64")]
        let stack_top_vpn = 1usize << 26;
        // RV32: 使用较小的地址空间 (Sv32: 20-bit VPN)
        #[cfg(target_pointer_width = "32")]
        let stack_top_vpn = 1usize << 19;
        
        address_space.map_extern(
            VPN::new(stack_top_vpn - 2)..VPN::new(stack_top_vpn),
            PPN::new(stack as usize >> VmMode::PAGE_BITS),
            VmFlags::build_from_str("U_WRV"),
        );

        log::info!("process entry = {:#x}", entry);

        let mut context = LocalContext::user(entry);
        
        // 根据架构构建 satp
        #[cfg(target_pointer_width = "64")]
        let satp = (8usize << 60) | address_space.root_ppn().val();
        #[cfg(target_pointer_width = "32")]
        let satp = (1usize << 31) | address_space.root_ppn().val();
        
        // 设置用户栈指针
        #[cfg(target_pointer_width = "64")]
        {
            *context.sp_mut() = 1 << 38;
        }
        #[cfg(target_pointer_width = "32")]
        {
            *context.sp_mut() = stack_top_vpn << VmMode::PAGE_BITS;
        }
        
        Some(Self {
            context: ForeignContext { context, satp },
            address_space,
        })
    }
}
