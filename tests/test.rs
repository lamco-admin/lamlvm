//! Integration test: open a real LVM PV image, find a linear LV, mount
//! ext4 on top of it, verify file contents. Mirrors the upstream test
//! from main--/rust-lvm2 with crate-name + I/O-trait updates for lamlvm.
//!
//! The test image `image.raw` is gitignored. Build a fresh one with:
//!
//! ```sh
//! sudo tests/build-fixture.sh
//! ```

use std::cell::RefCell;
use std::fs::File;
use std::io::Read as _;

use embedded_io::{Read, Seek, SeekFrom};
use embedded_io_adapters::std::FromStd;
use ext4::Options;
use lamlvm::Lvm2;
use positioned_io::ReadAt;
use snafu::ResultExt;
use tracing::Level;

type EioFile = FromStd<File>;

#[test]
fn ext4_on_linear_lv() -> Result<(), snafu::Whatever> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .try_init();

    let path = "image.raw";
    if !std::path::Path::new(path).exists() {
        eprintln!(
            "skipping ext4_on_linear_lv: {path} not present. \
             Build it via `sudo tests/build-fixture.sh`."
        );
        return Ok(());
    }

    let mut f1: EioFile =
        FromStd::new(File::open(path).whatever_context("opening test image (PV parse)")?);
    let lvm = Lvm2::open(&mut f1).whatever_context("parsing PV")?;
    let lv = lvm.lvs().next().expect("test image has at least one LV");
    tracing::info!("LV {}", lv.name());

    // Re-open the file for the LV reader so the borrow chain stays clean.
    let f2: EioFile =
        FromStd::new(File::open(path).whatever_context("re-opening for LV reader")?);
    let mut olv = lvm.open_lv(lv, f2);

    let mut buf = [0u8; 1024];
    olv.read_exact(&mut buf).whatever_context("read prologue")?;

    let mut options = Options::default();
    options.checksums = ext4::Checksums::Enabled;
    let e4 = ext4::SuperBlock::new_with_options(Wrapper(RefCell::new(olv)), &options)
        .whatever_context("mount ext4")?;
    assert_eq!(read_file(&e4, "/testfile1"), "foo\n");
    assert_eq!(read_file(&e4, "/testfile2"), "bar\n");

    Ok(())
}

fn read_file(
    e4: &ext4::SuperBlock<Wrapper<lamlvm::OpenLV<'_, EioFile>>>,
    name: &str,
) -> String {
    let entry = e4.resolve_path(name).unwrap();
    let inode = e4.load_inode(entry.inode).unwrap();
    let mut ino = e4.open(&inode).unwrap();
    let mut bytes = Vec::new();
    ino.read_to_end(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

/// Adapter from our `embedded_io::Read + Seek` types to the `positioned_io::ReadAt`
/// trait that the `ext4` crate consumes.
struct Wrapper<T>(RefCell<T>);

impl<T: Read + Seek> ReadAt for Wrapper<T> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut this = self.0.borrow_mut();
        this.seek(SeekFrom::Start(pos))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{e:?}")))?;
        this.read(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{e:?}")))
    }
}
