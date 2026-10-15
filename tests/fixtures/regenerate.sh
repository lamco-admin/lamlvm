#!/bin/bash
# Build the lamlvm fixture images with the LVM2 tools on loop devices.
#
#   tests/fixtures/regenerate.sh [FIXTURE...]
#
# With no arguments every fixture is rebuilt. Each PV image is committed as
# NAME.pvN.img.zst, and MANIFEST holds one line per LV the kernel could read:
# "NAME|LV|SIZE|SHA256" of the LV's bytes as device-mapper shows them, plus a
# comment per fixture. Needs lvm2, zstd, python3 and sudo. LVM's devices file
# must be off (use_devicesfile=0, the Debian default), or pvcreate on a loop
# device would add it there.
set -euo pipefail
export PATH="/usr/sbin:/sbin:$PATH"
cd "$(dirname "$0")"
FX="$PWD"
MAN=MANIFEST
ALL="wrapped_metadata no_timestamp striped_and_thin"
# No thin_check on the build host; the kernel's dm-thin-pool is enough here.
THIN='global { thin_check_executable = "" }'

WORK=$(mktemp -d "${TMPDIR:-/var/tmp}/lamlvm-fixtures.XXXXXX")
LOOPS=()
VG=""
cleanup() {
  if [ -n "$VG" ]; then sudo vgchange -an --config "$THIN" "$VG" >/dev/null 2>&1 || true; fi
  local l
  for l in "${LOOPS[@]}"; do sudo losetup -d "$l" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

# attach NAME N SIZE: N fresh PV images NAME.pv0.img..., each on a loop device.
attach() {
  local name=$1 n=$2 size=$3 i
  LOOPS=()
  for i in $(seq 0 $((n - 1))); do
    rm -f "$name.pv$i.img" "$name.pv$i.img.zst"
    truncate -s "$size" "$name.pv$i.img"
    LOOPS+=("$(sudo losetup --find --show "$FX/$name.pv$i.img")")
  done
}

detach() {
  sudo vgchange -an --config "$THIN" "$VG" >/dev/null
  local l
  for l in "${LOOPS[@]}"; do sudo losetup -d "$l"; done
  LOOPS=()
  VG=""
}

# fill VG LV TEXT: write a repeating pattern over the whole LV.
fill() {
  local bytes
  bytes=$(sudo blockdev --getsize64 "/dev/$1/$2")
  head -c "$bytes" < <(yes "$3") | sudo dd of="/dev/$1/$2" bs=1M iflag=fullblock conv=fsync status=none
}

# record NAME VG LV: the LV's size and hash as the kernel reads it.
record() {
  local dev="/dev/$2/$3"
  echo "$1|$3|$(sudo blockdev --getsize64 "$dev")|$(sudo sha256sum "$dev" | cut -d' ' -f1)" >> "$MAN"
}

note() { echo "# $1 $2" >> "$MAN"; }
finish() { local f; for f in "$1".pv*.img; do zstd -q -19 -f --rm "$f"; done; }

# Where the committed metadata text sits in the first PV's ring buffer:
# prints "start size area" in bytes, from the label and area headers.
metadata_locn() {
  python3 - "$1" <<'PY'
import struct, sys
img = open(sys.argv[1], 'rb')
img.seek(512)
label = img.read(512)
assert label[:8] == b'LABELONE'
pvh = 512 + struct.unpack_from('<I', label, 20)[0]
img.seek(pvh)
hdr = img.read(512)
at = 40
while struct.unpack_from('<Q', hdr, at)[0]:    # data areas, zero-terminated
    at += 16
at += 16
mda = struct.unpack_from('<Q', hdr, at)[0]     # first metadata area
img.seek(mda)
mh = img.read(512)
area = struct.unpack_from('<Q', mh, 32)[0]
off, size = struct.unpack_from('<QQ', mh, 40)
print(off, size, area)
PY
}

# metadata_text IMG: the committed text, joined across the wrap.
metadata_text() {
  python3 - "$1" <<'PY'
import struct, sys
img = open(sys.argv[1], 'rb')
img.seek(512)
label = img.read(512)
pvh = 512 + struct.unpack_from('<I', label, 20)[0]
img.seek(pvh)
hdr = img.read(512)
at = 40
while struct.unpack_from('<Q', hdr, at)[0]:
    at += 16
at += 16
mda = struct.unpack_from('<Q', hdr, at)[0]
img.seek(mda)
mh = img.read(512)
area = struct.unpack_from('<Q', mh, 32)[0]
off, size = struct.unpack_from('<QQ', mh, 40)
first = min(size, area - off)
img.seek(mda + off)
text = img.read(first)
img.seek(mda + 512)
text += img.read(size - first)
sys.stdout.write(text.rstrip(b'\0').decode())
PY
}

# A long-lived VG: tag changes rewrite the metadata until the committed copy
# runs past the end of the 1 MiB ring and continues at its start.
build_wrapped_metadata() {
  local name=wrapped_metadata i off size area
  VG=lamlvm_wrap
  attach "$name" 1 24M
  sudo pvcreate -q --metadatasize 1m "${LOOPS[0]}" >/dev/null
  sudo vgcreate -q -s 1m "$VG" "${LOOPS[0]}" >/dev/null
  sudo lvcreate -q -y -n root -L 8m "$VG" >/dev/null
  fill "$VG" root "lamlvm wrapped-metadata root"
  for i in $(seq 1 4000); do
    sudo lvchange -q --addtag "t$i" "$VG/root" >/dev/null
    sudo lvchange -q --deltag "t$i" "$VG/root" >/dev/null
    read -r off size area < <(metadata_locn "$name.pv0.img")
    [ $((off + size)) -gt "$area" ] && break
  done
  [ $((off + size)) -gt "$area" ] || { echo "$name: metadata never wrapped" >&2; exit 1; }
  record "$name" "$VG" root
  note "$name" "committed text at $off+$size in a $area-byte area: wraps after $((area - off)) bytes"
  detach
  finish "$name"
}

# An LV whose metadata has no creation_time or creation_host, as LVM2 writes
# for an LV without a timestamp, beside one that has them.
build_no_timestamp() {
  local name=no_timestamp
  VG=lamlvm_nots
  attach "$name" 1 24M
  sudo pvcreate -q "${LOOPS[0]}" >/dev/null
  sudo vgcreate -q -s 1m "$VG" "${LOOPS[0]}" >/dev/null
  sudo lvcreate -q -y -n stamped -L 4m "$VG" >/dev/null
  sudo lvcreate -q -y -n unstamped -L 4m "$VG" >/dev/null
  fill "$VG" stamped "lamlvm stamped LV"
  fill "$VG" unstamped "lamlvm unstamped LV"
  sudo vgchange -an -q "$VG" >/dev/null
  sudo vgcfgbackup -q -f "$WORK/vg.cfg" "$VG" >/dev/null
  sudo chown "$(id -u)" "$WORK/vg.cfg"
  python3 - "$WORK/vg.cfg" <<'PY'
import re, sys
path = sys.argv[1]
text = open(path).read()
start = text.index('\t\tunstamped {')
end = text.index('\n\t\t}\n', start)
block = re.sub(r'\n\t+creation_(time|host) = [^\n]*', '', text[start:end])
open(path, 'w').write(text[:start] + block + text[end:])
PY
  sudo vgcfgrestore -q -f "$WORK/vg.cfg" "$VG" >/dev/null
  metadata_text "$name.pv0.img" > "$WORK/text"
  python3 - "$WORK/text" <<'PY'
import sys
text = open(sys.argv[1]).read()
block = text[text.index('unstamped {'):]
block = block[:block.index('segment1')]
assert 'creation_time' not in block and 'creation_host' not in block, 'timestamp survived'
PY
  sudo vgchange -ay -q "$VG" >/dev/null
  record "$name" "$VG" stamped
  record "$name" "$VG" unstamped
  note "$name" "LV unstamped has no creation_time or creation_host in the on-disk metadata"
  detach
  finish "$name"
}

# A linear root next to a two-stripe LV and a thin pool with a thin LV, on two
# PVs. Only the root reads; the others must fail alone.
build_striped_and_thin() {
  local name=striped_and_thin
  VG=lamlvm_mixed
  attach "$name" 2 32M
  sudo pvcreate -q "${LOOPS[@]}" >/dev/null
  sudo vgcreate -q -s 1m "$VG" "${LOOPS[@]}" >/dev/null
  sudo lvcreate -q -y -n root -L 6m "$VG" "${LOOPS[0]}" >/dev/null
  sudo lvcreate -q -y -n striped -i 2 -I 64k -L 8m "$VG" >/dev/null
  sudo lvcreate -q -y --config "$THIN" --type thin-pool -n pool -L 8m "$VG" >/dev/null
  sudo lvcreate -q -y --config "$THIN" -n thin -V 16m --thinpool "$VG/pool" >/dev/null
  fill "$VG" root "lamlvm linear root"
  fill "$VG" striped "lamlvm striped LV"
  record "$name" "$VG" root
  note "$name" "LVs striped (2 stripes), pool (thin-pool) and thin (thin) must fail to open on their own"
  detach
  finish "$name"
}

[ $# -gt 0 ] || set -- $ALL
touch "$MAN"
echo "generating fixtures:"
for name in "$@"; do
  grep -vE "^($name\||# $name )" "$MAN" > "$WORK/man" || true
  cat "$WORK/man" > "$MAN"
  "build_$name"
  echo "  $name -> $(du -ch "$name".pv*.img.zst | tail -n 1 | cut -f1)"
done
