//! The virtual keyboard every physical keyboard is proxied through.
//!
//! Written against the raw uinput ioctls because the evdev crate's builder
//! cannot declare LEDs: without them the compositor never tells us the Caps or
//! Num Lock state, and the physical keyboards' LEDs would stop following it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use tokio::io::unix::AsyncFd;

pub const NAME: &str = "catlock virtual keyboard";

const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_MSCBIT: libc::c_ulong = 0x4004_5568;
const UI_SET_LEDBIT: libc::c_ulong = 0x4004_5569;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_MSC: u16 = 0x04;
pub const EV_LED: u16 = 0x11;
const MSC_SCAN: u16 = 0x04;
const BUS_VIRTUAL: u16 = 0x06;

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

const EVENT_SIZE: usize = std::mem::size_of::<RawEvent>();

pub struct VirtualKeyboard {
    fd: AsyncFd<File>,
}

fn ioctl(file: &File, request: libc::c_ulong, arg: libc::c_ulong) -> io::Result<()> {
    // SAFETY: every request used here takes an integer or a pointer to a live struct.
    match unsafe { libc::ioctl(file.as_raw_fd(), request, arg) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

impl VirtualKeyboard {
    pub fn create() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open("/dev/uinput")?;
        ioctl(&file, UI_SET_EVBIT, EV_KEY.into())?;
        // Keyboard keys only: declaring BTN_* codes would make libinput treat
        // the device as a mouse or touchscreen.
        for code in (1..=0xff).chain(0x160..=0x2bf) {
            ioctl(&file, UI_SET_KEYBIT, code)?;
        }
        ioctl(&file, UI_SET_EVBIT, EV_MSC.into())?;
        ioctl(&file, UI_SET_MSCBIT, MSC_SCAN.into())?;
        ioctl(&file, UI_SET_EVBIT, EV_LED.into())?;
        for led in 0..=4 {
            ioctl(&file, UI_SET_LEDBIT, led)?;
        }

        let mut setup = UinputSetup {
            bustype: BUS_VIRTUAL,
            vendor: 0x1209,
            product: 0xca70,
            version: 1,
            name: [0; 80],
            ff_effects_max: 0,
        };
        setup.name[..NAME.len()].copy_from_slice(NAME.as_bytes());
        ioctl(&file, UI_DEV_SETUP, &setup as *const _ as libc::c_ulong)?;
        ioctl(&file, UI_DEV_CREATE, 0)?;
        // SAFETY: the File owns its descriptor and lives inside the AsyncFd.
        let fd = unsafe { AsyncFd::register(file) }.map_err(|e| io::Error::other(format!("{e:?}")))?;
        Ok(Self { fd })
    }

    /// Writes one report: the given (type, code, value) events and a SYN_REPORT.
    pub fn emit(&self, events: &[(u16, u16, i32)]) -> io::Result<()> {
        let zero = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let mut buf = Vec::with_capacity((events.len() + 1) * EVENT_SIZE);
        for &(kind, code, value) in events.iter().chain([&(EV_SYN, 0, 0)]) {
            let ev = RawEvent { time: zero, kind, code, value };
            // SAFETY: RawEvent is plain old data with the kernel's layout.
            buf.extend_from_slice(unsafe {
                std::slice::from_raw_parts(&ev as *const RawEvent as *const u8, EVENT_SIZE)
            });
        }
        self.fd.get_ref().write_all(&buf)
    }

    /// Waits for LED changes the compositor writes to the virtual keyboard.
    pub async fn read_leds(&self) -> io::Result<Vec<(u16, i32)>> {
        loop {
            let mut guard = self.fd.readable().await?;
            let mut buf = [0u8; EVENT_SIZE * 16];
            match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
                Ok(Ok(n)) => {
                    return Ok(buf[..n]
                        .as_chunks::<EVENT_SIZE>()
                        .0
                        .iter()
                        .map(|chunk| {
                            // SAFETY: the kernel writes whole input_event structs.
                            unsafe { std::ptr::read_unaligned(chunk.as_ptr() as *const RawEvent) }
                        })
                        .filter(|ev| ev.kind == EV_LED)
                        .map(|ev| (ev.code, ev.value))
                        .collect());
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}
