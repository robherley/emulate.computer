export function diskSeed(manifest: {
  disk: { hash: string; id: string; logicalBytes: number };
  files: { "rootfs.ext4.gz": { url: string } };
}) {
  if (!/^[a-f0-9]{64}$/.test(manifest.disk.hash)
      || manifest.disk.id !== `sha256:${manifest.disk.hash}`)
    throw new Error("Invalid guest disk identity");
  return { ...manifest.disk, url: manifest.files["rootfs.ext4.gz"].url };
}
