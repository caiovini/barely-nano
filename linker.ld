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

  /* Stack: 512KB, grows downward */
  . = ALIGN(16);
  _stack_top = . + 0x80000;
  . = _stack_top;

  /* Heap: 4MB */
  _heap_start = .;
  . = . + 0x400000;
  _heap_end = .;
}