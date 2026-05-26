//! Example: open an LVM PV (real block device or image file), pick an LV
//! by name, mount ext4 on it via the same ext4-view + lamlvm stack
//! lamboot-core ships, and read two fixture files.
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

use std::env;
use std::error::Error;
use std::fs::File;

use embedded_io::{Read, Seek, SeekFrom};
use embedded_io_adapters::std::FromStd;
use ext4_view::{Ext4, Ext4Read};
use lamlvm::{Lvm2, OwnedLvReader};
use snafu::ResultExt;

type EioFile = FromStd<File>;

fn main() -> Result<(), snafu::Whatever> {
    tracing_subscriber::fmt().init();

    let mut args = env::args().skip(1);
    let pv_path = args
        .next()
        .ok_or("usage: walk_ext4_on_lv <pv_path> <lv_name>")
        .whatever_context("missing pv_path arg")?;
    let lv_name = args
        .next()
        .ok_or("usage: walk_ext4_on_lv <pv_path> <lv_name>")
        .whatever_context("missing lv_name arg")?;

    let mut f1: EioFile = FromStd::new(File::open(&pv_path).whatever_context("opening PV")?);
    let lvm = Lvm2::open(&mut f1).whatever_context("parsing PV")?;
    tracing::info!(vg = lvm.vg_name(), pv = lvm.pv_name(), "opened LVM");

    let f2: EioFile = FromStd::new(File::open(&pv_path).whatever_context("re-opening PV")?);
    let owned = lvm
        .open_lv_owned_by_name(&lv_name, f2)
        .map_err(|e| format!("open_lv_owned: {e:?}"))
        .whatever_context("LV owned-open")?
        .ok_or_else(|| format!("no LV named {lv_name:?} in VG {}", lvm.vg_name()))
        .whatever_context("looking up LV")?;
    tracing::info!(lv = %lv_name, bytes = owned.len(), "opened LV");

    let ext4 = Ext4::load(Box::new(LvExt4Adapter { lv: owned }))
        .map_err(|e| format!("ext4 load: {e}"))
        .whatever_context("mount ext4")?;
    tracing::info!(
        uuid = %ext4.uuid(),
        label = ?ext4.label().to_str().ok(),
        "mounted ext4 on LV"
    );

    let entries: Vec<_> = ext4
        .read_dir(ext4_view::Path::new("/"))
        .map_err(|e| format!("read_dir /: {e}"))
        .whatever_context("read_dir /")?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().as_str().ok().map(String::from))
        .filter(|n| n != "." && n != "..")
        .collect();
    for name in &entries {
        tracing::info!(%name, "/");
    }

    Ok(())
}

struct LvExt4Adapter {
    lv: OwnedLvReader<EioFile>,
}

impl Ext4Read for LvExt4Adapter {
    fn read(
        &mut self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
        self.lv
            .seek(SeekFrom::Start(start_byte))
            .map_err(|e| -> Box<dyn Error + Send + Sync> { format!("seek: {e:?}").into() })?;
        let mut filled = 0;
        while filled < dst.len() {
            let n = self
                .lv
                .read(&mut dst[filled..])
                .map_err(|e| -> Box<dyn Error + Send + Sync> { format!("read: {e:?}").into() })?;
            if n == 0 {
                return Err("EOF before read_exact completed".into());
            }
            filled += n;
        }
        Ok(())
    }
}
