#!/bin/sh
# Container-side half of build-rootfs.sh: $1 is the flattened root tree
# produced by the `rootfs-tar` stage of guest/Dockerfile, $2 the ext4 image to
# write.
#
# Runs inside a pinned native-architecture Alpine container so macOS needs
# neither root nor host e2fsprogs. For reproducibility every mtime/atime is
# stamped to SOURCE_DATE_EPOCH, every owner normalized, and mke2fs runs with a
# fixed UUID, label and hash seed under E2FSPROGS_FAKE_TIME.

set -eu

tarball="$1"
image="$2"
epoch="${SOURCE_DATE_EPOCH:-1751241600}"
stamp="${ROOTFS_TOUCH_STAMP:-202506300000.00}"
guest_hostname="emulate.computer"

apk add --no-cache e2fsprogs=1.47.2-r2 >/dev/null

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/root"
tar xf "$tarball" -C "$work/root"

# The three files the Docker daemon bind-mounts over every RUN, so the Dockerfile
# cannot write them: they only exist here.
rm -f "$work/root/etc/resolv.conf"
printf '%s\n' "$guest_hostname" > "$work/root/etc/hostname"
cat > "$work/root/etc/hosts" <<EOF
127.0.0.1	localhost localhost.localdomain $guest_hostname
::1		localhost localhost.localdomain $guest_hostname
EOF
chmod 0644 "$work/root/etc/hostname" "$work/root/etc/hosts"

# Normalize everything with build-time metadata.
# chown clears setuid/setgid bits, so record those modes before normalization
# and replay them afterwards.
setid_modes="$work/setid-modes"
find "$work/root" -type f \( -perm -4000 -o -perm -2000 \) \
  -exec stat -c '%a %n' {} + > "$setid_modes"
chown -R 0:0 "$work/root"
while read -r mode path; do
  [ -n "$mode" ] || continue
  chmod "$mode" "$path"
done < "$setid_modes"
rm -f "$setid_modes"
# The fontconfig caches in this tree were built by guest/Dockerfile against
# font directories already stamped with this value; restamping them to anything
# else invalidates every cache, and each Xft client then rescans 48 gzipped
# bitmap fonts in-process — 22 s of guest CPU on the first `emuctl desktop`.
recorded="$(cat "$work/root/etc/emulate-rootfs-stamp" 2>/dev/null || echo missing)"
if [ "$recorded" != "$stamp" ]; then
  echo "error: rootfs mtime stamp mismatch: image says '$recorded', this script" >&2
  echo "  uses '$stamp'. Set ROOTFS_STAMP in guest/Dockerfile and" >&2
  echo "  ROOTFS_TOUCH_STAMP here to the same value." >&2
  exit 2
fi

TZ=UTC find "$work/root" -exec touch -h -t "$stamp" {} +

# Everything below runs against the *final* tree, so it catches a normalization
# step that undoes what the Dockerfile set up.
test -x "$work/root/usr/bin/python3"
runtime_elf_machine="$(od -An -tu2 -j18 -N2 "$work/root/usr/bin/python3" | tr -d ' ')"
if [ "$runtime_elf_machine" != "243" ]; then
  echo "error: Python binary is not RISC-V ELF (e_machine=$runtime_elf_machine)" >&2
  exit 2
fi

# The console logs in as root, so bash and its home files must be present before
# the image is sealed.
test -x "$work/root/bin/bash"
test -x "$work/root/usr/bin/qjs"
test -x "$work/root/usr/local/bin/emuctl"
grep -q '^root:x:0:0:root:/root:/bin/bash$' "$work/root/etc/passwd"

SOURCE_DATE_EPOCH="$epoch" E2FSPROGS_FAKE_TIME="$epoch" \
  mke2fs -q -F -t ext4 -b 4096 -m 0 \
  -L emulate-root -U 5e9f7e1a-5865-4f89-8a01-8ab52cc6b574 \
  -E root_owner=0:0,lazy_itable_init=0,lazy_journal_init=0,hash_seed=5e9f7e1a-5865-4f89-8a01-8ab52cc6b574 \
  -d "$work/root" "$image"
e2fsck -fn "$image"
