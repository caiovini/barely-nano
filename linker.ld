ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 128M
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

  /* Stack: 8MB (1MB per Hart) */
  . = ALIGN(16);
  . = . + 0x800000;
  _stack_top = .;

  /* Heap: 64MB */
  _heap_start = .;
  . = . + 0x4000000;
  _heap_end = .;
}