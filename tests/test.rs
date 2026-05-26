//! Integration test: open a real LVM PV image, find a linear LV, mount
//! ext4 on top of it, verify file contents. Mirrors the upstream test
//! from main--/rust-lvm2 with crate-name + I/O-trait updates for lamlvm.
//!
//! The test image `image.raw` is gitignored. Build a fresh one with:
//!
//! ```sh
//! dd if=/dev/zero of=image.raw bs=1M count=64
//! losetup --find --show image.raw                              # → /dev/loopN
//! pvcreate /dev/loopN
//! vgcreate testvg /dev/loopN
//! lvcreate -L 16M -n testlv testvg
//! mkfs.ext4 /dev/testvg/testlv
//! mount /dev/testvg/testlv /mnt
//! echo foo > /mnt/testfile1 ; echo bar > /mnt/testfile2
//! umount /mnt
//! lvchange -an testvg ; vgchange -an testvg
//! losetup -d /dev/loopN
//! ```

use std::cell::RefCell;
use std::fs::File;

use embedded_io::{Read, Seek, SeekFrom};
use ext4::Options;
use lamlvm::Lvm2;
use positioned_io::ReadAt;
use snafu::ResultExt;
use tracing::Level;

#[test]
fn ext4_on_linear_lv() -> Result<(), snafu::Whatever> {
    tracing_subscriber::fmt().with_max_level(Level::TRACE).init();

    // Use the image fixture if present; skip with a helpful message otherwise
    // so `cargo test` works out-of-the-box for crate consumers.
    let path = "image.raw";
    if !std::path::Path::new(path).exists() {
        eprintln!(
            "skipping ext4_on_linear_lv: {path} not present. See test-file \
             header comment for how to build it."
        );
        return Ok(());
    }

    let mut f = File::open(path).whatever_context("opening test image")?;
    let lvm = Lvm2::open(&mut f).whatever_context("parsing PV")?;
    let lv = lvm.lvs().next().expect("test image has at least one LV");
    tracing::info!("LV {}", lv.name());
    let mut olv = lvm.open_lv(lv, &mut f);

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
    e4: &ext4::SuperBlock<Wrapper<lamlvm::OpenLV<&mut File>>>,
    name: &str,
) -> String {
    let entry = e4.resolve_path(name).unwrap();
    let inode = e4.load_inode(entry.inode).unwrap();
    let mut ino = e4.open(&inode).unwrap();
    let mut s = String::new();
    ino.read_to_string(&mut s).unwrap();
    s
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
