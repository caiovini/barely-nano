.section .text.start
.global _start
_start:
    # a0 already contains hart_id from OpenSBI/Bootloader
    # Store hart_id in tp for later use in trap handlers
    # Sanitize hart_id: only allow 0-7
    li   t0, 7
    and  a0, a0, t0
    mv   tp, a0
    
    # Set up stack: _stack_top - (hart_id * 32MB)
    la   sp, _stack_top
    li   t0, 25           # Shift left by 25 for 32MB
    sll  t1, a0, t0
    sub  sp, sp, t1

    # Call rust_main
    call rust_main

    # If rust_main returns, park the hart
park:
    wfi
    j park
