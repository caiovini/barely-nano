.section .text.start
.global _start
_start:
    # a0 already contains hart_id from OpenSBI/Bootloader
    
    # Set up stack: _stack_top - (hart_id * 2MB)
    la   sp, _stack_top
    li   t0, 21           # Shift left by 21 for 2MB
    sll  t1, a0, t0
    sub  sp, sp, t1

    # Jump to rust_main
    j    rust_main
