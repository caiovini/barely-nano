/* S-mode: Kernel loads at 0x104000000 (SpacemiT K3 Pico-ITX 16 GB LPDDR5 layout) */
ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x104000000, LENGTH = 3392M
}

SECTIONS
{
  .text : {
    *(.text.start)
    *(.text*)
  } > RAM

  .rodata : { *(.rodata*) } > RAM

  .data : { *(.data*) } > RAM

  .bss : {
    _bss_start = .;
    *(.bss*)
    *(COMMON)
    _bss_end = .;
  } > RAM

  /* Stack: 256MB (16MB per Hart across 16 Harts: 0x108000000 .. 0x118000000) */
  . = ALIGN(4096);
  . = 0x108000000;
  _stack_bottom = .;
  . = . + 0x10000000;
  _stack_top = .;

  /* Heap start at 0x118000000; 3.00GB span (0x118000000 .. 0x1D8000000) is sized via soc::HEAP_END
   * to avoid >2GB R_RISCV_PCREL_HI20 relocation overflow from 0x104000000. */
  . = ALIGN(4096);
  . = 0x118000000;
  _heap_start = .;
}
