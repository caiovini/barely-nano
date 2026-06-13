ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x11000000, LENGTH = 1024M
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

  /* Stack: 256MB (32MB per Hart) starting 16MB after base */
  . = ALIGN(4096);
  . = 0x12000000;
  _stack_bottom = .;
  . = . + 0x10000000;
  _stack_top = .;

  /* Heap: 512MB starting ~240MB after base */
  . = ALIGN(4096);
  . = 0x20000000;
  _heap_start = .;
  . = . + 0x20000000;
  _heap_end = .;
}