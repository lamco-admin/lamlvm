#!/bin/bash
# Build the integration-test fixture `image.raw` from scratch using
# losetup + LVM userspace tools. One-time setup; the resulting image is
# .gitignored and consumed by `cargo test` (and by manual experiments
# from `cargo run --example walk_ext4_on_lv`).
#
# Requires sudo for losetup / pvcreate / vgcreate / lvcreate / mkfs.ext4 /
# mount — all root-only operations on Debian-family systems.
#
# Usage:
#   cd ~/lamlvm-dev
#   sudo tests/build-fixture.sh

set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE="${PWD}/image.raw"
SIZE_MB=64
LV_SIZE_MB=16
VG_NAME="testvg"
LV_NAME="testlv"

if [[ ${EUID} -ne 0 ]]; then
    echo "error: must run as root (sudo). Need losetup / lvm / mkfs / mount." >&2
    exit 1
fi

# If a previous run left a loop device or active VG, clean it up first.
cleanup_prior() {
    # Look for any existing testvg activation, deactivate, then look for
    # loop devices backed by ${IMAGE} and detach them.
    if vgs --noheadings -o vg_name 2>/dev/null | grep -qw "${VG_NAME}"; then
        echo "[cleanup] deactivating existing ${VG_NAME}..."
        vgchange -an "${VG_NAME}" 2>/dev/null || true
        vgremove -ff "${VG_NAME}" 2>/dev/null || true
    fi
    # Detach any loop device pointing at IMAGE
    losetup -j "${IMAGE}" 2>/dev/null | cut -d: -f1 | while read -r LDEV; do
        if [[ -n "${LDEV:-}" ]]; then
            echo "[cleanup] detaching ${LDEV}..."
            losetup -d "${LDEV}" || true
        fi
    done
}

cleanup_prior

# Build a fresh sparse image of SIZE_MB
if [[ -f "${IMAGE}" ]]; then
    echo "[1/8] removing stale ${IMAGE}"
    rm -f "${IMAGE}"
fi
echo "[1/8] creating ${IMAGE} (${SIZE_MB}M sparse)..."
truncate -s "${SIZE_MB}M" "${IMAGE}"

echo "[2/8] attaching loop device..."
LDEV=$(losetup --find --show "${IMAGE}")
echo "       ${LDEV}"

trap '
    set +e
    umount /mnt/lamlvm-fixture 2>/dev/null
    rmdir  /mnt/lamlvm-fixture 2>/dev/null
    vgchange -an "${VG_NAME}" 2>/dev/null
    vgremove -ff "${VG_NAME}" 2>/dev/null
    losetup -d "${LDEV}" 2>/dev/null
' EXIT

echo "[3/8] pvcreate ${LDEV}..."
pvcreate -ff -y "${LDEV}" >/dev/null

echo "[4/8] vgcreate ${VG_NAME}..."
vgcreate "${VG_NAME}" "${LDEV}" >/dev/null

echo "[5/8] lvcreate -L ${LV_SIZE_MB}M -n ${LV_NAME}..."
lvcreate -L "${LV_SIZE_MB}M" -n "${LV_NAME}" "${VG_NAME}" >/dev/null

echo "[6/8] mkfs.ext4 /dev/${VG_NAME}/${LV_NAME}..."
mkfs.ext4 -q "/dev/${VG_NAME}/${LV_NAME}"

echo "[7/8] populating fixture files..."
mkdir -p /mnt/lamlvm-fixture
mount "/dev/${VG_NAME}/${LV_NAME}" /mnt/lamlvm-fixture
echo "foo" > /mnt/lamlvm-fixture/testfile1
echo "bar" > /mnt/lamlvm-fixture/testfile2
sync
umount /mnt/lamlvm-fixture
rmdir  /mnt/lamlvm-fixture

echo "[8/8] tearing down LVM + loop..."
vgchange -an "${VG_NAME}"
# vgremove not needed — the image will keep its LVM2 metadata so the
# crate can re-open it from cold bytes.
losetup -d "${LDEV}"
trap - EXIT

# Make readable by the invoking user (not root) so `cargo test` works
# without sudo.
SUDO_USER_REAL="${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [[ -n "${SUDO_USER_REAL:-}" ]]; then
    chown "${SUDO_USER_REAL}:" "${IMAGE}"
fi
chmod 644 "${IMAGE}"

echo ""
echo "done: ${IMAGE} ($(du -h "${IMAGE}" | cut -f1)) — VG=${VG_NAME} LV=${LV_NAME}"
echo "run: cargo test --test test  (no sudo needed)"
