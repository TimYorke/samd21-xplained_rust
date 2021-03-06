#![no_std]
#![no_main]

use bsp::ehal;
use bsp::hal;
use feather_m4 as bsp;

#[cfg(not(feature = "use_semihosting"))]
use panic_halt as _;
#[cfg(feature = "use_semihosting")]
use panic_semihosting as _;

use bsp::entry;
use ehal::digital::v2::ToggleableOutputPin;
use hal::clock::GenericClockController;
use hal::pac::{interrupt, CorePeripherals, Peripherals};
use hal::{pukcc::*, usb::UsbBus};

use usb_device::bus::UsbBusAllocator;
use usb_device::prelude::*;
use usbd_serial::{SerialPort, USB_CLASS_CDC};

use cortex_m::asm::delay as cycle_delay;
use cortex_m::peripheral::NVIC;

#[entry]
fn main() -> ! {
    let mut peripherals = Peripherals::take().unwrap();
    let mut core = CorePeripherals::take().unwrap();
    let mut clocks = GenericClockController::with_external_32kosc(
        peripherals.GCLK,
        &mut peripherals.MCLK,
        &mut peripherals.OSC32KCTRL,
        &mut peripherals.OSCCTRL,
        &mut peripherals.NVMCTRL,
    );
    let pins = bsp::Pins::new(peripherals.PORT);
    let mut red_led = pins.d13.into_push_pull_output();

    let bus_allocator = unsafe {
        USB_ALLOCATOR = Some(bsp::usb_allocator(
            pins.usb_dm,
            pins.usb_dp,
            peripherals.USB,
            &mut clocks,
            &mut peripherals.MCLK,
        ));
        USB_ALLOCATOR.as_ref().unwrap()
    };

    unsafe {
        USB_SERIAL = Some(SerialPort::new(&bus_allocator));
        USB_BUS = Some(
            UsbDeviceBuilder::new(&bus_allocator, UsbVidPid(0x16c0, 0x27dd))
                .manufacturer("Fake company")
                .product("Serial port")
                .serial_number("TEST")
                .device_class(USB_CLASS_CDC)
                .build(),
        );
    }

    unsafe {
        core.NVIC.set_priority(interrupt::USB_OTHER, 1);
        core.NVIC.set_priority(interrupt::USB_TRCPT0, 1);
        core.NVIC.set_priority(interrupt::USB_TRCPT1, 1);
        NVIC::unmask(interrupt::USB_OTHER);
        NVIC::unmask(interrupt::USB_TRCPT0);
        NVIC::unmask(interrupt::USB_TRCPT1);
    }

    let pukcc = Pukcc::enable(&mut peripherals.MCLK).unwrap();

    loop {
        serial_writeln!("Column 1: Is generated signature identical to a reference signature?",);
        serial_writeln!("Column 2: Is a signature valid according to PUKCC");
        serial_writeln!("Column 3: Is a broken signature invalid according to PUKCC");
        serial_writeln!("Test vector: {} samples", K_SIGNATURE_PAIRS.len());
        for (i, (k, reference_signature)) in K_SIGNATURE_PAIRS.iter().enumerate() {
            let i = i + 1;
            let mut generated_signature = [0_u8; 64];
            let are_signatures_same = match unsafe {
                pukcc.zp_ecdsa_sign_with_raw_k::<curves::Nist256p>(
                    &mut generated_signature,
                    &SIGNED_HASH,
                    &PRIVATE_KEY,
                    k,
                )
            } {
                Ok(_) => generated_signature
                    .iter()
                    .zip(reference_signature.iter())
                    .map(|(&left, &right)| left == right)
                    .all(|r| r == true),
                Err(e) => {
                    serial_writeln!("Error during signature generation: {:?}", e);
                    false
                }
            };
            let is_signature_valid = match pukcc.zp_ecdsa_verify_signature::<curves::Nist256p>(
                &generated_signature,
                &SIGNED_HASH,
                &PUBLIC_KEY,
            ) {
                Ok(_) => true,
                Err(_) => false,
            };

            // Break signature
            generated_signature[14] = generated_signature[14].wrapping_sub(1);

            let is_broken_signature_invalid = match pukcc
                .zp_ecdsa_verify_signature::<curves::Nist256p>(
                    &generated_signature,
                    &SIGNED_HASH,
                    &PUBLIC_KEY,
                ) {
                Err(_) => true,
                Ok(_) => false,
            };
            serial_writeln!(
                "{:>2}: {:<5} | {:<5} | {:<5}",
                i,
                are_signatures_same,
                is_signature_valid,
                is_broken_signature_invalid,
            );
        }

        cycle_delay(5 * 1024 * 1024);
        red_led.toggle().ok();
    }
}

static mut USB_ALLOCATOR: Option<UsbBusAllocator<UsbBus>> = None;
static mut USB_BUS: Option<UsbDevice<UsbBus>> = None;
static mut USB_SERIAL: Option<SerialPort<UsbBus>> = None;

/// Borrows the global singleton `UsbSerial` for a brief period with interrupts
/// disabled
///
/// # Arguments
/// `borrower`: The closure that gets run borrowing the global `UsbSerial`
///
/// # Safety
/// the global singleton `UsbSerial` can be safely borrowed because we disable
/// interrupts while it is being borrowed, guaranteeing that interrupt handlers
/// like `USB` cannot mutate `UsbSerial` while we are as well.
///
/// # Panic
/// If `init` has not been called and we haven't initialized our global
/// singleton `UsbSerial`, we will panic.
fn usbserial_get<T, R>(borrower: T) -> R
where
    T: Fn(&mut SerialPort<UsbBus>) -> R,
{
    usb_free(|_| unsafe {
        let mut usb_serial = USB_SERIAL.as_mut().expect("UsbSerial not initialized");
        borrower(&mut usb_serial)
    })
}

/// Execute closure `f` in an interrupt-free context.
///
/// This as also known as a "critical section".
#[inline]
fn usb_free<F, R>(f: F) -> R
where
    F: FnOnce(&cortex_m::interrupt::CriticalSection) -> R,
{
    NVIC::mask(interrupt::USB_OTHER);
    NVIC::mask(interrupt::USB_TRCPT0);
    NVIC::mask(interrupt::USB_TRCPT1);

    let r = f(unsafe { &cortex_m::interrupt::CriticalSection::new() });

    unsafe {
        NVIC::unmask(interrupt::USB_OTHER);
        NVIC::unmask(interrupt::USB_TRCPT0);
        NVIC::unmask(interrupt::USB_TRCPT1);
    };

    r
}

/// Writes the given message out over USB serial.
///
/// # Arguments
/// * println args: variable arguments passed along to `core::write!`
///
/// # Warning
/// as this function deals with a static mut, and it is also accessed in the
/// USB interrupt handler, we both have unsafe code for unwrapping a static mut
/// as well as disabling of interrupts while we do so.
///
/// # Safety
/// the only time the static mut is used, we have interrupts disabled so we know
/// we have sole access
#[macro_export]
macro_rules! serial_writeln {
    ($($tt:tt)+) => {{
        use core::fmt::Write;

        let mut s: heapless::String<256> = heapless::String::new();
        core::write!(&mut s, $($tt)*).unwrap();
        usbserial_get(|usbserial| {
            usbserial.write(s.as_bytes()).ok();
            usbserial.write("\r\n".as_bytes()).ok();
        });
    }};
}

fn poll_usb() {
    unsafe {
        USB_BUS.as_mut().map(|usb_dev| {
            USB_SERIAL.as_mut().map(|serial| {
                usb_dev.poll(&mut [serial]);
                let mut buf = [0u8; 64];

                if let Ok(count) = serial.read(&mut buf) {
                    for (i, c) in buf.iter().enumerate() {
                        if i >= count {
                            break;
                        }
                        serial.write(&[c.clone()]).unwrap();
                    }
                };
            });
        });
    };
}

#[interrupt]
fn USB_OTHER() {
    poll_usb();
}

#[interrupt]
fn USB_TRCPT0() {
    poll_usb();
}

#[interrupt]
fn USB_TRCPT1() {
    poll_usb();
}

const PRIVATE_KEY: [u8; 32] = [
    0x30, 0x8d, 0x6c, 0x77, 0xcc, 0x43, 0xf7, 0xb8, 0x4f, 0x44, 0x74, 0xdc, 0x2f, 0x99, 0xf6, 0x33,
    0x3e, 0x26, 0x8a, 0xc, 0x94, 0x4c, 0xde, 0x56, 0xff, 0xb5, 0x27, 0xb7, 0x7f, 0xa6, 0x11, 0xc,
];
const PUBLIC_KEY: [u8; 64] = [
    0x16, 0xa6, 0xbd, 0x9a, 0x66, 0x66, 0x36, 0xd0, 0x72, 0x86, 0xde, 0x78, 0xb9, 0xa1, 0xe7, 0xf6,
    0xdd, 0x67, 0x75, 0xb2, 0xc6, 0xf4, 0x2c, 0xcf, 0x83, 0x2d, 0xe4, 0x5e, 0x1e, 0x22, 0x9d, 0x84,
    0xa, 0xca, 0xd, 0xdd, 0xe8, 0xf5, 0xc8, 0x2f, 0x84, 0x10, 0xb5, 0x62, 0xc2, 0x3a, 0x46, 0xde,
    0xcd, 0xcb, 0x59, 0x6e, 0x40, 0x2, 0xcb, 0x10, 0xc6, 0x2f, 0x5b, 0x5e, 0xb5, 0xf2, 0xa7, 0xd7,
];
const SIGNED_HASH: [u8; 32] = [
    0xc7, 0x9a, 0x27, 0x4d, 0x91, 0xbb, 0x92, 0x9f, 0x29, 0x16, 0xf8, 0x9c, 0xb2, 0xa6, 0xec, 0x66,
    0xa0, 0xcd, 0xb4, 0x4a, 0x14, 0x97, 0x63, 0x65, 0x3f, 0x28, 0x8, 0x52, 0xbb, 0xa5, 0x3b, 0xe,
];
const K_SIGNATURE_PAIRS: [([u8; 32], [u8; 64]); 10] = [
    (
        [
            0x48, 0x4a, 0x19, 0x66, 0x2, 0x50, 0xe, 0xf2, 0xd0, 0xbe, 0x90, 0x84, 0x23, 0x8e, 0x45,
            0x9, 0x6c, 0x23, 0x8b, 0x1b, 0x74, 0xa8, 0x6b, 0x17, 0x46, 0x62, 0x75, 0xd2, 0xfa,
            0x27, 0x7e, 0x1b,
        ],
        [
            0x21, 0xea, 0xf, 0xfd, 0x35, 0x43, 0xdf, 0x7a, 0xdb, 0xf5, 0x4f, 0x88, 0xe, 0x9d, 0xd2,
            0xa7, 0x26, 0x4f, 0x2f, 0x96, 0xe9, 0x85, 0x5f, 0x67, 0xa9, 0x82, 0x46, 0xfe, 0x46,
            0xef, 0x92, 0x9d, 0x3c, 0x59, 0x7c, 0x22, 0x4b, 0x69, 0x80, 0xf7, 0x1, 0x46, 0x9, 0xce,
            0x13, 0x59, 0xfd, 0x21, 0xd1, 0x45, 0x65, 0xfb, 0xb0, 0x82, 0x1b, 0x91, 0xce, 0x1e,
            0x87, 0xf5, 0xe5, 0xc8, 0xdc, 0x9c,
        ],
    ),
    (
        [
            0xea, 0x40, 0xe8, 0x9d, 0xf6, 0x63, 0xf4, 0x3e, 0x71, 0xf2, 0x6b, 0x7f, 0xcd, 0xa0,
            0x15, 0x59, 0x13, 0x4f, 0xa9, 0x17, 0xbd, 0x5f, 0xbc, 0xf3, 0x36, 0xfb, 0x48, 0x14,
            0x8f, 0x59, 0x99, 0x1d,
        ],
        [
            0x9a, 0x84, 0x64, 0x3b, 0xd1, 0xb8, 0xe2, 0xa6, 0xe3, 0xc7, 0x96, 0x9b, 0xfa, 0x0,
            0xac, 0x65, 0x19, 0xa8, 0x3e, 0x22, 0x2e, 0x40, 0x7d, 0x90, 0x98, 0x92, 0xce, 0x3b,
            0x77, 0x4e, 0x8c, 0x41, 0xe7, 0xa1, 0xcd, 0xb1, 0xc4, 0xa, 0xc0, 0x73, 0xfa, 0x87,
            0x5f, 0xa5, 0xae, 0xcf, 0x27, 0x14, 0x6, 0x38, 0x9f, 0x4c, 0x7f, 0xaa, 0xf9, 0x76,
            0x6e, 0x49, 0x3, 0xc, 0xc8, 0x33, 0x26, 0x3,
        ],
    ),
    (
        [
            0x99, 0xde, 0xf2, 0x6b, 0xa6, 0xfe, 0x92, 0xf, 0xd6, 0x33, 0x3a, 0x1b, 0x21, 0x2c,
            0xcb, 0xd2, 0x50, 0x81, 0x57, 0xad, 0x26, 0x31, 0xea, 0x56, 0x23, 0x94, 0x69, 0x3b,
            0xc3, 0xe7, 0x96, 0xd7,
        ],
        [
            0x47, 0x1a, 0x16, 0x6b, 0xde, 0x2e, 0x34, 0xb3, 0xc6, 0x80, 0xa2, 0x18, 0xed, 0xa7,
            0xfa, 0xc6, 0x7f, 0xfc, 0x77, 0xae, 0x80, 0xce, 0x18, 0x90, 0x51, 0x1f, 0x4d, 0x23,
            0x8a, 0x96, 0x62, 0x25, 0xa7, 0x5a, 0xc7, 0x47, 0x68, 0xa2, 0xf0, 0x76, 0x5e, 0x1,
            0x6b, 0x29, 0xb2, 0x9d, 0xba, 0x3b, 0x71, 0x8a, 0x7c, 0xfd, 0xaa, 0x49, 0x53, 0xe0,
            0x90, 0x62, 0xce, 0x6, 0x95, 0x55, 0xd4, 0xc4,
        ],
    ),
    (
        [
            0x91, 0xda, 0x2c, 0xea, 0x22, 0xc3, 0x8, 0x44, 0x5c, 0x1, 0xe, 0x2b, 0x0, 0x74, 0x44,
            0x5, 0x14, 0x50, 0x25, 0x92, 0xb3, 0xde, 0xe9, 0xcd, 0xb0, 0x67, 0x25, 0x10, 0x26,
            0x8a, 0x66, 0xb6,
        ],
        [
            0x89, 0xaa, 0x32, 0x68, 0x8, 0xbf, 0x3f, 0xd8, 0xbb, 0x13, 0xc5, 0x51, 0xa6, 0xe, 0x13,
            0x3f, 0xb5, 0x6f, 0x96, 0xcd, 0x7d, 0x9f, 0xe7, 0xd4, 0x17, 0xef, 0xad, 0x93, 0x14,
            0xed, 0x4f, 0xf, 0xdb, 0x34, 0xc1, 0xc3, 0xf4, 0xc9, 0x11, 0x9e, 0xd7, 0xe7, 0x23,
            0xbc, 0xd3, 0x5c, 0x73, 0x57, 0xd5, 0x74, 0x75, 0x90, 0xaf, 0x4e, 0x60, 0x47, 0x57,
            0xe0, 0x16, 0xc2, 0xd, 0x9e, 0xce, 0x44,
        ],
    ),
    (
        [
            0x3d, 0x3d, 0x65, 0x81, 0x9d, 0xc3, 0xd1, 0x23, 0xde, 0x2d, 0xe0, 0x92, 0x99, 0x7d,
            0xb, 0xb5, 0xab, 0x93, 0x2, 0xa, 0x8b, 0xd, 0x37, 0xe3, 0xe0, 0xf, 0xf7, 0x91, 0x60,
            0x39, 0xf4, 0x97,
        ],
        [
            0x56, 0x99, 0xc2, 0x70, 0x77, 0x34, 0x71, 0x9a, 0xdb, 0xcf, 0xb3, 0xc1, 0xa, 0x5d,
            0x2a, 0x18, 0xbd, 0x35, 0xcc, 0x46, 0x6c, 0xfb, 0x87, 0xa, 0xe2, 0xc2, 0x6f, 0xdf,
            0x23, 0x70, 0x2c, 0x49, 0xc0, 0xd7, 0x2a, 0x54, 0xf6, 0xd6, 0x46, 0x5f, 0xb0, 0x59,
            0xe0, 0x70, 0x58, 0xae, 0x64, 0x9c, 0x3f, 0x2d, 0x48, 0xad, 0xf6, 0x66, 0xe9, 0x3,
            0x88, 0xf7, 0xa, 0xe, 0x2a, 0xec, 0xba, 0x12,
        ],
    ),
    (
        [
            0x4e, 0xa9, 0xce, 0xda, 0xce, 0xe2, 0xe9, 0x58, 0x43, 0xcd, 0x90, 0x70, 0x75, 0xc6,
            0xe8, 0x58, 0x19, 0x74, 0x9, 0xa, 0x75, 0xa3, 0xfb, 0xbd, 0x38, 0x97, 0xba, 0x92, 0xb3,
            0x87, 0x81, 0x88,
        ],
        [
            0x4a, 0x20, 0xe6, 0xf3, 0xf0, 0x96, 0xc4, 0xad, 0x6b, 0xe4, 0x95, 0xae, 0xeb, 0xee,
            0xa9, 0xb8, 0x90, 0x45, 0x87, 0xfb, 0x32, 0x8e, 0x30, 0xce, 0x49, 0xaa, 0x11, 0x7f,
            0x11, 0x2a, 0xba, 0xa1, 0x54, 0xe0, 0xb3, 0x68, 0x25, 0x76, 0x5c, 0xf9, 0xb, 0x46,
            0xdf, 0x8d, 0x8b, 0x99, 0x1b, 0x9d, 0x2d, 0x9f, 0xfb, 0x52, 0xcc, 0x32, 0xb2, 0x4c,
            0x2a, 0x93, 0xff, 0x23, 0xe5, 0xf7, 0x88, 0x9f,
        ],
    ),
    (
        [
            0x8d, 0xbc, 0xea, 0x73, 0x54, 0x1b, 0x93, 0x29, 0xc6, 0xb6, 0x82, 0xbc, 0xd2, 0xa6,
            0xeb, 0x68, 0x9, 0x9c, 0x64, 0x97, 0x34, 0xc9, 0x3c, 0xe8, 0x56, 0xd7, 0x3a, 0xdb,
            0x32, 0x78, 0xb6, 0xa,
        ],
        [
            0x91, 0x1a, 0x54, 0x4e, 0xc3, 0x77, 0x3b, 0x37, 0x48, 0x84, 0xbc, 0x84, 0xc2, 0x6d,
            0x32, 0x9b, 0xc9, 0x5f, 0x24, 0x7c, 0x4d, 0x29, 0xc7, 0xd6, 0xd5, 0x23, 0xf5, 0x25,
            0x49, 0x6f, 0xf0, 0xd3, 0xa3, 0xf, 0xf7, 0x2a, 0xa9, 0x86, 0xb1, 0xd1, 0xf0, 0x31,
            0xa5, 0x71, 0x40, 0x8c, 0xc4, 0xf, 0x56, 0xa0, 0x8c, 0x4b, 0xfc, 0x7d, 0xe5, 0x98,
            0x90, 0xdb, 0xcd, 0x68, 0xe9, 0x4b, 0x4f, 0x9c,
        ],
    ),
    (
        [
            0x2b, 0xe8, 0x71, 0x47, 0x76, 0xc, 0x8e, 0x96, 0x7c, 0xcf, 0x2b, 0x78, 0xc2, 0x89,
            0xbd, 0xef, 0x8d, 0x2f, 0x7a, 0xe7, 0xa0, 0xab, 0x8b, 0x84, 0xa8, 0x43, 0xe6, 0x33,
            0x36, 0x67, 0xcd, 0x8,
        ],
        [
            0x6d, 0xa1, 0x3e, 0xf9, 0xf0, 0x53, 0x89, 0x67, 0xb0, 0xf4, 0xe3, 0x86, 0xb3, 0x56,
            0x7a, 0x9a, 0xcf, 0xba, 0x94, 0xb8, 0xba, 0xbf, 0xb6, 0xa0, 0x7f, 0xaa, 0xc4, 0xd8,
            0xcb, 0x2c, 0x3a, 0xf4, 0x11, 0xc4, 0x3a, 0x17, 0x52, 0x10, 0x5d, 0xde, 0x72, 0x5d,
            0x5a, 0xc1, 0xd9, 0x3a, 0x5f, 0x56, 0xb8, 0x79, 0x78, 0x4c, 0x71, 0xb3, 0x5, 0x69,
            0x52, 0x63, 0x6, 0xe8, 0xe3, 0xe2, 0xfa, 0x10,
        ],
    ),
    (
        [
            0x87, 0x2f, 0x9, 0x31, 0x90, 0xcf, 0xeb, 0x70, 0x96, 0x9a, 0x67, 0x59, 0xa9, 0x8b,
            0x40, 0xe3, 0xfe, 0xfd, 0xeb, 0x37, 0x53, 0xcf, 0x62, 0xfe, 0x27, 0x13, 0x7b, 0xe6,
            0x8, 0xf0, 0x3d, 0x1c,
        ],
        [
            0x7c, 0x93, 0x91, 0x44, 0x1e, 0x84, 0x7, 0xc2, 0x22, 0xd3, 0x92, 0x3b, 0xb0, 0xfe,
            0x41, 0xe8, 0x8d, 0x1e, 0x1d, 0xd5, 0x56, 0x56, 0x5, 0x31, 0x44, 0xc8, 0xa2, 0x4b,
            0xee, 0xdc, 0x8c, 0x36, 0x3f, 0xc5, 0xe9, 0xfa, 0x57, 0x1e, 0x20, 0x5f, 0xc5, 0x97,
            0xd1, 0xe8, 0x84, 0xaf, 0x74, 0x10, 0xc6, 0x8, 0x6b, 0x3e, 0xea, 0x61, 0x7c, 0x9a,
            0x77, 0x54, 0x31, 0x8b, 0x3b, 0x8b, 0x4, 0xc5,
        ],
    ),
    (
        [
            0xca, 0x8b, 0x60, 0xf1, 0x88, 0x6d, 0xb6, 0xf7, 0x33, 0x4f, 0xcc, 0x39, 0x9c, 0xf4,
            0x82, 0xe7, 0xde, 0x42, 0x37, 0x8d, 0xb9, 0x97, 0xa6, 0x5e, 0x4b, 0xe, 0xc8, 0xaa, 0x9,
            0xbc, 0xee, 0xac,
        ],
        [
            0x69, 0xc1, 0x7c, 0x3f, 0xaa, 0x6, 0xa, 0x0, 0xd, 0xdf, 0x8, 0x1d, 0x38, 0xe3, 0xa8,
            0xc5, 0xe7, 0xa5, 0x80, 0xbd, 0x48, 0x27, 0xdf, 0x20, 0x12, 0x36, 0x9f, 0x4b, 0xfd,
            0x59, 0x2a, 0x92, 0x95, 0x71, 0xa3, 0xc, 0x58, 0x7d, 0x6a, 0x6d, 0x7b, 0x1f, 0xc1,
            0x43, 0x5a, 0x6d, 0x55, 0x8e, 0xe0, 0xc5, 0x76, 0xc6, 0xf0, 0xdc, 0x92, 0x77, 0x23,
            0x14, 0x49, 0xb6, 0xa6, 0xe5, 0x1d, 0x1c,
        ],
    ),
];
