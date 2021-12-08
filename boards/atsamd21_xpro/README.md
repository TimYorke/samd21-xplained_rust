# SAM D21 Xplained Pro Evaluation Kit Board Support Crate

This crate provides a type-safe Rust API for working with the
[SAM D21 Xplained Pro Evaluation Kit](https://www.microchip.com/developmenttools/productdetails/atsamd21-xpro).

## Board Features

- Microchip [ATSAMD21J] Cortex-M4 microcontroller @ 48 MHz (32-bit, 3.3V logic and power)
  - 256KB Flash
  - 32kB SRAM
  - 8Mbit SPI Flash chip

## Prerequisites
* Install the cross compile toolchain `rustup target add thumbv6m-none-eabi`

## Examplea
Check out the repository for examples:

https://github.com/atsamd-rs/atsamd/tree/master/boards/atsamd21_xpro/examples
