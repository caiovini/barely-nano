/* S-mode: Kernel loads at 0x11000000 (Orange Pi RV2 hardware layout) */
ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x11000000, LENGTH = 1792M
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

  /* Stack: 256MB (32MB per Hart) */
  . = ALIGN(4096);
  . = 0x12000000;
  _stack_bottom = .;
  . = . + 0x10000000;
  _stack_top = .;

  /* Heap: ~1.45GB - stops before U-Boot's reserved area at 0x7dd8b000 */
  . = ALIGN(4096);
  . = 0x22000000;
  _heap_start = .;
  . = . + 0x5B000000;
  _heap_end = .;
}
