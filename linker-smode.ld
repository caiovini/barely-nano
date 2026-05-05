/* S-mode: OpenSBI sits at 0x80000000, our kernel loads at 0x80200000 */
ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 1024M
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
  . = 0x81000000;
  _stack_bottom = .;
  . = . + 0x10000000;
  _stack_top = .;

  /* Heap: 512MB */
  . = ALIGN(4096);
  . = 0xA0000000;
  _heap_start = .;
  . = . + 0x20000000;
  _heap_end = .;
}
