/* S-mode: OpenSBI sits at 0x80000000, our kernel loads at 0x80200000 */
ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 510M
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

  /* Stack: 16MB total (2MB per hart), grows downward */
  . = ALIGN(16);
  _stack_top = . + 0x1000000;
  . = _stack_top;

  /* Heap: 400MB */
  _heap_start = .;
  . = . + 0x19000000;
  _heap_end = .;
}
