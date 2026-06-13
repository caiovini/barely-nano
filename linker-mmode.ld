ENTRY(_start);

MEMORY
{
  RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 2048M
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

  /* Heap: 1536MB (1.5GB) */
  . = ALIGN(4096);
  . = 0xA0000000;
  _heap_start = .;
  . = . + 0x60000000;
  _heap_end = .;
}