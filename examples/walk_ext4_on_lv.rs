//! Example: open an LVM PV (real block device or image file), pick an LV
//! by name, mount ext4 on it, and walk the filesystem printing every
//! discovered path.
//!
//! Usage:
//!
//! ```sh
//! # against a real LVM device:
//! cargo run --example walk_ext4_on_lv -- /dev/sda3 root
//!
//! # against an image fixture:
//! cargo run --example walk_ext4_on_lv -- image.raw testlv
//! ```

use std::cell::RefCell;
use std::env;
use std::fs::File;

use embedded_io::{Read, Seek, SeekFrom};
use ext4::Options;
use lamlvm::Lvm2;
use positioned_io::ReadAt;
use snafu::ResultExt;
use tracing::Level;

fn main() -> Result<(), snafu::Whatever> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let mut args = env::args().skip(1);
    let pv_path = args
        .next()
        .ok_or("usage: walk_ext4_on_lv <pv_path> <lv_name>")
        .whatever_context("missing pv_path arg")?;
    let lv_name = args
        .next()
        .ok_or("usage: walk_ext4_on_lv <pv_path> <lv_name>")
        .whatever_context("missing lv_name arg")?;

    let mut f = File::open(&pv_path).whatever_context("opening PV")?;
    let lvm = Lvm2::open(&mut f).whatever_context("parsing PV")?;
    tracing::info!(vg = lvm.vg_name(), pv = lvm.pv_name(), "opened LVM");

    let olv = lvm
        .open_lv_by_name(&lv_name, &mut f)
        .ok_or_else(|| format!("no LV named {lv_name:?} in VG {}", lvm.vg_name()))
        .whatever_context("looking up LV")?;

    let mut options = Options::default();
    options.checksums = ext4::Checksums::Enabled;
    let e4 = ext4::SuperBlock::new_with_options(Wrapper(RefCell::new(olv)), &options)
        .whatever_context("mount ext4")?;
    let root = e4.root().whatever_context("ext4 root inode")?;
    e4.walk(&root, "/", &mut |_, path, _, _| {
        tracing::info!(%path);
        Ok(true)
    })
    .whatever_context("ext4 walk")?;

    Ok(())
}

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
