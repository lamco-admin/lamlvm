//! Integration test: open a real LVM PV image, find a linear LV, mount
//! ext4 on it via `ext4-view`, and read fixture files. This is the same
//! crate-stack lamboot-core ships in `fs_backend_lvm`, so a green test
//! here validates the production read path end-to-end.
//!
//! The test image `image.raw` is gitignored. Build a fresh one with:
//!
//! ```sh
//! sudo tests/build-fixture.sh
//! ```

use std::error::Error;
use std::fs::File;

use embedded_io::{Read, Seek, SeekFrom};
use embedded_io_adapters::std::FromStd;
use ext4_view::{Ext4, Ext4Read};
use lamlvm::{Lvm2, OwnedLvReader};

type EioFile = FromStd<File>;

#[test]
fn ext4_on_linear_lv_via_ext4_view() {
    let path = "image.raw";
    if !std::path::Path::new(path).exists() {
        eprintln!(
            "skipping ext4_on_linear_lv_via_ext4_view: {path} not present. \
             Build it via `sudo tests/build-fixture.sh`."
        );
        return;
    }

    // Parse the PV + VG metadata.
    let mut f0: EioFile = FromStd::new(File::open(path).expect("open image.raw (parse)"));
    let lvm = Lvm2::open(&mut f0).expect("parse PV");
    let lv = lvm.lvs().next().expect("test fixture has at least one LV");
    let lv_name = lv.name().to_string();
    eprintln!("opened VG={} LV={}", lvm.vg_name(), lv_name);

    // Open the LV with the owned reader (the API lamboot-core uses).
    let f_lv: EioFile = FromStd::new(File::open(path).expect("open image.raw (LV)"));
    let owned = lvm
        .open_lv_owned_by_name(&lv_name, f_lv)
        .expect("owned-open errored")
        .expect("LV not found via owned API");

    eprintln!(
        "owned LV reader: {} bytes ({} extents)",
        owned.len(),
        owned.len() / lvm.extent_size().max(1),
    );

    // Mount ext4 on the LV bytes via ext4-view, the same way
    // `lamboot-core::fs_backend_lvm::LvmExt4Backend` does.
    let ext4 = Ext4::load(Box::new(LvExt4Adapter { lv: owned })).expect("ext4-view load");
    eprintln!(
        "mounted ext4: uuid={} label={:?}",
        ext4.uuid(),
        ext4.label().to_str().ok()
    );

    // Verify fixture file contents via two different access patterns to
    // exercise both the inode-walk and the directory-iterator paths.
    let testfile1 = ext4
        .read(ext4_view::Path::new("/testfile1"))
        .expect("read /testfile1");
    assert_eq!(testfile1, b"foo\n");

    let testfile2 = ext4
        .read(ext4_view::Path::new("/testfile2"))
        .expect("read /testfile2");
    assert_eq!(testfile2, b"bar\n");

    // Confirm both files show up in / via the directory iterator (which
    // exercises a different chunk of the LV byte stream).
    let mut names: Vec<String> = ext4
        .read_dir(ext4_view::Path::new("/"))
        .expect("read_dir /")
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().as_str().ok().map(String::from))
        .filter(|n| n != "." && n != ".." && n != "lost+found")
        .collect();
    names.sort();
    assert_eq!(names, vec!["testfile1".to_string(), "testfile2".to_string()]);
}

/// `OwnedLvReader` → `Ext4Read` adapter. Identical pattern to
/// `lamboot-core/src/fs_backend_lvm.rs::LvExt4Adapter`. Kept inline here
/// rather than imported so this integration test stays self-contained.
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
            .map_err(|e| -> Box<dyn Error + Send + Sync> {
                format!("seek: {e:?}").into()
            })?;
        let mut filled = 0;
        while filled < dst.len() {
            let n = self
                .lv
                .read(&mut dst[filled..])
                .map_err(|e| -> Box<dyn Error + Send + Sync> {
                    format!("read: {e:?}").into()
                })?;
            if n == 0 {
                return Err("EOF before read_exact completed".into());
            }
            filled += n;
        }
        Ok(())
    }
}
