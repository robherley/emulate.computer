import { $ } from "bun";

process.chdir(`${import.meta.dir}/..`);
$.cwd(process.cwd());
const input = "web/public/guest";
const output = "web/generated-public";
const guestFiles = ["fw.bin", "kernel.bin", "initrd.bin", "dtb.bin", "dtb-desktop.bin", "dtb-rootfs.bin", "rootfs.ext4.gz", "snapshot.bin.gz"];
const hash = (bytes: Uint8Array) => new Bun.CryptoHasher("sha256").update(bytes).digest("hex");

async function copyPublic() {
  await $`mkdir -p ${output}`;
  const entries = new Bun.Glob("*");
  for await (const name of entries.scan({ cwd: output, onlyFiles: false, dot: true })) {
    if (name !== "guest") await $`rm -rf ${`${output}/${name}`}`;
  }
  for await (const name of entries.scan({ cwd: "web/public", onlyFiles: false, dot: true })) {
    if (name !== "guest") await $`cp -R ${`web/public/${name}`} ${`${output}/${name}`}`;
  }
}

async function prepareGuest() {
  const files = new Map(await Promise.all(guestFiles.map(async name =>
    [name, Buffer.from(await Bun.file(`${input}/${name}`).arrayBuffer())] as const)));
  const diskHash = (await Bun.file(`${input}/rootfs.sha256`).text()).trim();
  const diskHasher = new Bun.CryptoHasher("sha256");
  let logicalBytes = 0;
  const disk = Bun.file(`${input}/rootfs.ext4.gz`).stream().pipeThrough(new DecompressionStream("gzip"));
  for await (const chunk of disk) {
    diskHasher.update(chunk);
    logicalBytes += chunk.length;
  }
  if (logicalBytes !== 536870912 || !/^[a-f0-9]{64}$/.test(diskHash) || diskHasher.digest("hex") !== diskHash)
    throw new Error("Rootfs size or checksum mismatch; run just guest-rootfs");

  const imageHasher = new Bun.CryptoHasher("sha256");
  for (const name of ["fw.bin", "kernel.bin", "initrd.bin", "dtb-desktop.bin"]) {
    const bytes = files.get(name)!;
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(bytes.length));
    imageHasher.update(length).update(bytes);
  }
  const diskId = `sha256:${diskHash}`;
  const snapshot = Buffer.from(Bun.gunzipSync(files.get("snapshot.bin.gz")!));
  if (snapshot.length < 173 || snapshot.subarray(0, 8).toString() !== "EMUSNAP1"
    || snapshot.readUInt32LE(50) !== diskId.length || snapshot.subarray(54, 125).toString() !== diskId
    || snapshot.subarray(18, 50).toString("hex") !== imageHasher.digest("hex"))
    throw new Error("Snapshot does not match the disk and boot images; run just guest-rootfs");
  for (let offset = 12; offset < snapshot.length;) {
    if (offset + 6 > snapshot.length) throw new Error("Truncated snapshot section");
    offset += 6 + snapshot.readUInt32LE(offset + 2);
    if (offset > snapshot.length) throw new Error("Truncated snapshot section");
  }

  const manifest = {
    disk: { id: diskId, hash: diskHash, logicalBytes },
    files: {} as Record<string, { url: string; bytes: number; sha256: string }>,
  };
  await $`rm -rf ${`${output}/guest`}`;
  await $`mkdir -p ${`${output}/guest`} web/src/generated`;
  for (const [name, bytes] of files) {
    const sha256 = hash(bytes);
    const filename = name.replace(".", `.${sha256}.`);
    manifest.files[name] = { url: `/guest/${filename}`, bytes: bytes.length, sha256 };
    await Bun.write(`${output}/guest/${filename}`, bytes);
  }
  await Bun.write("web/src/generated/guest.json", JSON.stringify(manifest, null, 2) + "\n");
}

switch (Bun.argv[2] ?? "local") {
  case "local":
    await $`wasm-pack build crates/emulate-wasm --target web --out-dir ../../web/src/wasm`;
    await prepareGuest();
    await $`npm run build --workspace web`;
    break;
  case "release":
    await $`bash scripts/release-assets.sh fetch`;
    await $`npm run build --workspace web`;
    break;
  case "prepare":
    await prepareGuest();
    await copyPublic();
    break;
  case "public":
    await copyPublic();
    break;
  default:
    throw new Error("Usage: bun scripts/build-site.ts [local|release|prepare|public]");
}
