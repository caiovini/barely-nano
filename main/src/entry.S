.macro PUTC char
    li   t1, \char
88:
    lbu  t2, 20(t0)
    andi t2, t2, 0x20
    beqz t2, 88b
    sb   t1, 0(t0)
    # Dummy read to flush APB and provide bus delay
    lbu  t2, 20(t0)
.endm

.macro PUT_T1
88:
    lbu  t2, 20(t0)
    andi t2, t2, 0x20
    beqz t2, 88b
    sb   t1, 0(t0)
    lbu  t2, 20(t0)
.endm

.section .text.entry
    .global _start
_start:
    # Save parameters across stack setup
    mv   s0, a0
    mv   s1, a1

    # Save hart_id in tp immediately (a0 = hartid from OpenSBI HSM)
    li   t0, 7
    and  t1, a0, t0
    mv   tp, t1

    # Write 1 to SECONDARY_HART_WAKE array at index tp using AMO
    # AMO bypasses L1 cache and is usually coherent across clusters on this SoC
    la   t0, SECONDARY_HART_WAKE
    slli t1, tp, 2
    add  t0, t0, t1
    li   t2, 1
    .option push
    .option arch, +a
    amoswap.w.rl zero, t2, 0(t0)
    .option pop
    .option push
    .option arch, +zicbom
    cbo.clean 0(t0)
    .option pop
    fence rw, rw

    # Disable interrupts
    csrw sie, zero

    # Install early trap vector IMMEDIATELY
    la   t0, early_trap
    csrw stvec, t0

    # Enable FS and VS in sstatus
    li   t0, (3 << 13) | (3 << 9)
    csrs sstatus, t0

    # U-Boot RISC-V ABI:
    # a0: argc (often used as hartid in standalone apps)
    # a1: argv
    # gp: pointer to global_data (gd_t)
    
    # Capture gp and load Jump Table immediately ONLY for Hart 1 (boot hart)
    li   s11, 0
    li   t0, 1
    bne  tp, t0, 1f      # Only Hart 1 expects gp from U-Boot
    mv   s10, gp
    beqz s10, 1f
    ld   s11, 264(s10)   # s11 = jt_ptr
1:
    
    # Set up stack: _stack_top - (hart_id * 32MB)
    la   sp, _stack_top
    li   t0, 25           # Shift left by 25 for 32MB
    sll  t1, tp, t0
    sub  sp, sp, t1

    # Restore parameters for rust_main
    mv   a0, tp           # Pass sanitized hart_id
    mv   a1, s1           # opaque
    mv   a2, s11          # Pass jt_ptr in a2
    call rust_main

    # If rust_main returns, park the hart
park:
    wfi
    j park

.align 4
early_trap:
    # Use direct MMIO to UART to print trap info
    lui  t0, 0xd4017
    slli t0, t0, 32
    srli t0, t0, 32

    # Print 'T'
    PUTC 0x54

    # Print hart ID
    mv   t1, tp
    addi t1, t1, 0x30
    PUT_T1

    # Print ':'
    PUTC 0x3a

    # Print scause in hex
    csrr s2, scause
    li   s3, 60          # Shift count: 15 * 4 = 60
4:
    srl  t1, s2, s3
    andi t1, t1, 0xf
    li   t5, 10
    blt  t1, t5, 5f
    addi t1, t1, 0x37    # 'A' - 10
    j    6f
5:
    addi t1, t1, 0x30    # '0'
6:
    PUT_T1

    addi s3, s3, -4
    bgez s3, 4b

    # Print '-'
    PUTC 0x2d

    # Print sepc in hex
    csrr s2, sepc
    li   s3, 60
9:
    srl  t1, s2, s3
    andi t1, t1, 0xf
    li   t5, 10
    blt  t1, t5, 10f
    addi t1, t1, 0x37
    j    11f
10:
    addi t1, t1, 0x30
11:
    PUT_T1

    addi s3, s3, -4
    bgez s3, 9b

    # Print newline
    PUTC 0x0a

early_trap_loop:
    wfi
    j    early_trap_loop

.global secondary_spin
secondary_spin:
    # Get hart ID (already in a0 from OpenSBI)
    li   t0, 7
    and  t1, a0, t0

    # Write 0xDEADBEEF to SECONDARY_HART_WAKE[hartid]
    la   t0, SECONDARY_HART_WAKE
    slli t1, t1, 2
    add  t0, t0, t1
    li   t2, 0xDEADBEEF
    
    .option push
    .option arch, +a
    amoswap.w.rl zero, t2, 0(t0)
    .option pop
    
    .option push
    .option arch, +zicbom
    cbo.clean 0(t0)
    .option pop

1:  wfi
    j 1b
