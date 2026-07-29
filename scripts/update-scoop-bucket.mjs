#!/usr/bin/env node
// Scoop bucket 清单一键更新：下载新发版便携 zip → 算 SHA256 → 改 ../scoop-bucket/bucket/kimicodebar.json
// 用法：node scripts/update-scoop-bucket.mjs 0.8.0   （或 npm run bump-bucket -- 0.8.0）
// 网络走 curl.exe（读 HTTPS_PROXY 环境变量）：GitHub 直连被重置时先 export HTTPS_PROXY=http://127.0.0.1:7897
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const bucketDir = process.env.KIMICODEBAR_BUCKET_DIR
  ? resolve(process.env.KIMICODEBAR_BUCKET_DIR)
  : resolve(root, "../scoop-bucket");
const version = process.argv[2];

if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("用法：node scripts/update-scoop-bucket.mjs <x.y.z>（如 0.8.0）");
  process.exit(1);
}

const url = `https://github.com/JYH1878/KimiCodeBar-Windows/releases/download/v${version}/KimiCodeBar_${version}_x64-portable.zip`;

// 下载到临时目录算哈希（Release 已 Publish 后才能跑，否则 404）
const tmp = mkdtempSync(join(tmpdir(), "kimicodebar-"));
const zipPath = join(tmp, "portable.zip");
try {
  console.log(`下载 ${url}`);
  execFileSync("curl.exe", ["-fSL", "--retry", "3", "-o", zipPath, url], { stdio: "inherit" });
  const hash = createHash("sha256").update(readFileSync(zipPath)).digest("hex");
  console.log(`SHA256 = ${hash}`);

  const manifestPath = join(bucketDir, "bucket", "kimicodebar.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.version === version) {
    console.log(`= 清单已是 ${version}，仅刷新哈希`);
  }
  manifest.version = version;
  manifest.architecture["64bit"].url = url;
  manifest.architecture["64bit"].hash = hash;
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 4) + "\n");
  console.log(`✓ 已更新 ${manifestPath}`);
  console.log(`\n后续（在 ${bucketDir} 下执行）：`);
  console.log(`  git add -A && git commit -m "chore: kimicodebar ${version}" && git push`);
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
