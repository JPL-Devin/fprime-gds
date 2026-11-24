//! Link adapter `uart`: the GDS reads and writes a serial device, 8 data bits, no parity, one stop bit, no
//! flow control, as `fprime-gds --comm-adapter uart` (pyserial's defaults).

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use fgds_core::{
    Adapter, Error, LinkReader, LinkWriter, Manifest, OptionSpec, PeerInfo, Plugin, PluginConfig, Registry, Result,
};
use rustix::fs::OFlags;
use rustix::termios::{self, ControlModes, InputModes, OptionalActions, QueueSelector, SpecialCodeIndex};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

pub struct UartPlugin;

impl Plugin for UartPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "comm-uart",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![
                OptionSpec {
                    name: "device",
                    help: "Serial device of the uart adapter",
                    default: Some("/dev/ttyACM0"),
                    secret: false,
                },
                OptionSpec {
                    name: "baud",
                    help: "Baud rate of the uart adapter",
                    default: Some("9600"),
                    secret: false,
                },
            ],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_adapter(
            "uart",
            Arc::new(|cfg| {
                let device = cfg.get("device").unwrap_or_default();
                Ok(Box::new(Uart { speed: speed(cfg)?, baud: cfg.get("baud").unwrap_or_default(), device }))
            }),
        )
    }
}

/// The rates a serial device can be set to; any other is refused when the GDS starts.
const RATES: [u32; 30] = [
    50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800,
    500000, 576000, 921600, 1000000, 1152000, 1500000, 2000000, 2500000, 3000000, 3500000, 4000000,
];

fn speed(cfg: &PluginConfig) -> Result<u32> {
    let text = cfg.get("baud").unwrap_or_else(|| "9600".into());
    text.parse::<u32>().ok().filter(|baud| RATES.contains(baud)).ok_or_else(|| {
        let rates: Vec<_> = RATES.iter().map(u32::to_string).collect();
        Error::other(format!("comm-uart baud: {text:?} is not one of {}", rates.join(", ")))
    })
}

struct Uart {
    device: String,
    baud: String,
    speed: u32,
}

type Halves = (Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo);

#[async_trait]
impl Adapter for Uart {
    async fn connect(&mut self) -> Result<Halves> {
        let port = open(&self.device, self.speed).map_err(|e| Error::other(format!("uart {}: {e}", self.device)))?;
        // The file is owned by the `AsyncFd` until both halves are dropped, which is what the newer, unsafe
        // constructor asks its caller to promise; unsafe code is not allowed in this workspace.
        #[allow(deprecated)]
        let port = AsyncFd::with_interest(port, Interest::READABLE | Interest::WRITABLE).map_err(Error::Io)?;
        let port = Arc::new(port);
        let peer = PeerInfo { address: format!("{}:{}bps", self.device, self.baud), message_boundaries: false };
        Ok((Box::new(Reader(port.clone())), Box::new(Writer(port)), peer))
    }
}

/// The device, not blocking, never the controlling terminal, in raw mode: every byte passes unchanged.
fn open(device: &str, speed: u32) -> io::Result<File> {
    let flags = OFlags::NOCTTY | OFlags::NONBLOCK;
    let port = OpenOptions::new().read(true).write(true).custom_flags(flags.bits() as i32).open(device)?;
    let mut settings = termios::tcgetattr(&port)?;
    settings.make_raw();
    settings.control_modes.remove(ControlModes::CSTOPB | ControlModes::CRTSCTS | ControlModes::PARENB);
    settings.control_modes.insert(ControlModes::CLOCAL | ControlModes::CREAD | ControlModes::CS8);
    settings.input_modes.remove(InputModes::IXON | InputModes::IXOFF | InputModes::IXANY);
    // One byte at least: a read with nothing to read says so (the device is not blocking) instead of returning 0,
    // which is the end of the device.
    settings.special_codes[SpecialCodeIndex::VMIN] = 1;
    settings.special_codes[SpecialCodeIndex::VTIME] = 0;
    settings.set_speed(speed)?;
    termios::tcsetattr(&port, OptionalActions::Now, &settings)?;
    termios::tcflush(&port, QueueSelector::IOFlush)?;
    Ok(port)
}

struct Reader(Arc<AsyncFd<File>>);

#[async_trait]
impl LinkReader for Reader {
    /// A device that is gone (unplugged, or the other end of a pseudo-terminal closed) ends the connection.
    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        let mut chunk = [0u8; 4096];
        loop {
            let mut ready = self.0.readable().await.map_err(Error::Io)?;
            match ready.try_io(|port| port.get_ref().read(&mut chunk)) {
                // No byte with the device readable: the end of the device, not an empty read.
                Ok(Ok(0)) => return Ok(0),
                Ok(Ok(count)) => {
                    buf.put_slice(&chunk[..count]);
                    return Ok(count);
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => {}
            }
        }
    }
}

struct Writer(Arc<AsyncFd<File>>);

#[async_trait]
impl LinkWriter for Writer {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let mut sent = 0;
        while sent < data.len() {
            let mut ready = self.0.writable().await.map_err(Error::Io)?;
            match ready.try_io(|port| port.get_ref().write(&data[sent..])) {
                Ok(Ok(0)) => return Err(Error::Io(io::ErrorKind::WriteZero.into())),
                Ok(Ok(count)) => sent += count,
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pseudo-terminal: the file of its controlling end and the path of the device.
    fn pty() -> (File, String) {
        let far = rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY).unwrap();
        rustix::pty::grantpt(&far).unwrap();
        rustix::pty::unlockpt(&far).unwrap();
        let path = rustix::pty::ptsname(&far, Vec::new()).unwrap().into_string().unwrap();
        (File::from(far), path)
    }

    fn adapter(device: &str, baud: &str) -> Result<Box<dyn Adapter>> {
        let mut registry = Registry::default();
        UartPlugin.register(&mut registry).unwrap();
        let mut cfg = PluginConfig::default();
        cfg.set("device", device);
        cfg.set("baud", baud);
        (registry.adapters["uart"].1)(&cfg)
    }

    #[tokio::test]
    async fn every_byte_value_passes_both_ways_unchanged() {
        let (mut far, device) = pty();
        let mut uart = adapter(&device, "115200").unwrap();
        let (mut reader, mut writer, peer) = uart.connect().await.unwrap();
        assert_eq!(peer.address, format!("{device}:115200bps"));
        // Every value, the ones a terminal would translate or act on included (0x03, 0x0a, 0x0d, 0x11, 0x13).
        let bytes: Vec<u8> = (0..=255).collect();
        writer.write(&bytes).await.unwrap();
        let mut got = vec![0u8; 256];
        far.read_exact(&mut got).unwrap();
        assert_eq!(got, bytes);

        far.write_all(&bytes).unwrap();
        let mut buf = BytesMut::new();
        while buf.len() < 256 {
            assert!(reader.read(&mut buf).await.unwrap() > 0);
        }
        assert_eq!(&buf[..], &bytes[..]);

        // The rate is the one asked for.
        let port = std::fs::File::open(&device).unwrap();
        assert_eq!(termios::tcgetattr(&port).unwrap().output_speed(), 115200);
    }

    #[tokio::test]
    async fn the_other_end_closing_ends_the_connection_and_the_next_one_opens_the_device_again() {
        let (far, device) = pty();
        let mut uart = adapter(&device, "9600").unwrap();
        let (mut reader, _writer, _) = uart.connect().await.unwrap();
        drop(far);
        let mut buf = BytesMut::new();
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), reader.read(&mut buf)).await.unwrap();
        assert!(matches!(ended, Ok(0) | Err(_)), "{ended:?}");
        assert!(uart.connect().await.is_err(), "the device is gone");
    }

    #[test]
    fn a_rate_no_device_has_and_a_missing_device_are_refused() {
        for baud in ["0", "-1", "abc", "12345", ""] {
            let refused = adapter("/dev/null", baud).err().expect(baud).to_string();
            assert!(refused.contains("is not one of"), "{refused}");
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut uart = adapter("/nonexistent/tty", "9600").unwrap();
        let refused = runtime.block_on(uart.connect()).err().unwrap().to_string();
        assert!(refused.contains("uart /nonexistent/tty"), "{refused}");
        // A file that is no terminal.
        let mut uart = adapter("/dev/null", "9600").unwrap();
        assert!(runtime.block_on(uart.connect()).is_err());
    }
}
