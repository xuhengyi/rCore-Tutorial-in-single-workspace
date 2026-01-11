# M-Mode entry point for -bios none boot (RV32 version)
# This code runs at 0x80000000 in M-Mode when QEMU starts with -bios none

    .section .text.m_entry
    .globl _m_start
_m_start:
    # Set up M-Mode stack
    la sp, m_stack_top
    # Save M-Mode sp to mscratch for trap handler
    csrw mscratch, sp

    # Set mstatus: MPP=01 (S-Mode), MPIE=1
    li t0, (1 << 11) | (1 << 7)
    csrw mstatus, t0

    # Set mepc to S-Mode entry point
    la t0, _start
    csrw mepc, t0

    # Set mtvec to M-Mode trap handler
    la t0, m_trap_vector
    csrw mtvec, t0

    # Delegate interrupts and exceptions to S-Mode except ecall from S-Mode
    # mideleg: delegate all interrupts to S-Mode
    li t0, 0xffff
    csrw mideleg, t0
    # medeleg: delegate all exceptions except ecall from S-Mode (bit 9)
    li t0, 0xffff
    li t1, (1 << 9)
    not t1, t1
    and t0, t0, t1
    csrw medeleg, t0

    # Set up PMP to allow S-Mode full access
    # pmpaddr0 = 0xffffffff (all address space for RV32)
    li t0, -1
    csrw pmpaddr0, t0
    # pmpcfg0 = 0x0f (TOR, RWX)
    li t0, 0x0f
    csrw pmpcfg0, t0

    # Enable S-Mode to access counters
    li t0, -1
    csrw mcounteren, t0

    # Jump to S-Mode
    mret

    .section .text.m_trap
    .globl m_trap_vector
    .align 4
m_trap_vector:
    # Swap sp with mscratch (save user sp, load M-Mode sp)
    csrrw sp, mscratch, sp
    # Allocate stack frame for saved registers (RV32: 4 bytes per register)
    addi sp, sp, -136
    # Save caller-saved registers
    sw ra, 0(sp)
    sw t0, 4(sp)
    sw t1, 8(sp)
    sw t2, 12(sp)
    sw t3, 16(sp)
    sw t4, 20(sp)
    sw t5, 24(sp)
    sw t6, 28(sp)
    sw a0, 32(sp)
    sw a1, 36(sp)
    sw a2, 40(sp)
    sw a3, 44(sp)
    sw a4, 48(sp)
    sw a5, 52(sp)
    sw a6, 56(sp)
    sw a7, 60(sp)
    # Save original sp from mscratch
    csrr t0, mscratch
    sw t0, 64(sp)

    # Call Rust trap handler
    # Arguments already in a0-a7 for SBI call
    call m_trap_handler

    # Advance mepc past ecall instruction (4 bytes)
    csrr t0, mepc
    addi t0, t0, 4
    csrw mepc, t0

    # Restore original sp to mscratch
    lw t0, 64(sp)
    csrw mscratch, t0
    # Restore caller-saved registers
    lw ra, 0(sp)
    lw t0, 4(sp)
    lw t1, 8(sp)
    lw t2, 12(sp)
    lw t3, 16(sp)
    lw t4, 20(sp)
    lw t5, 24(sp)
    lw t6, 28(sp)
    # Note: a0, a1 contain return values, don't restore them
    lw a2, 40(sp)
    lw a3, 44(sp)
    lw a4, 48(sp)
    lw a5, 52(sp)
    lw a6, 56(sp)
    lw a7, 60(sp)
    # Deallocate stack frame
    addi sp, sp, 136
    # Restore sp from mscratch
    csrrw sp, mscratch, sp
    # Return from M-Mode trap
    mret

    .section .bss.m_stack
    .globl m_stack_lower_bound
m_stack_lower_bound:
    .space 4096 * 4    # 16KB M-Mode stack
    .globl m_stack_top
m_stack_top:
