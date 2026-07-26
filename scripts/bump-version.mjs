#!/usr/bin/env node
// 版本号一键同步：package.json / tauri.conf.json / Cargo.toml / Cargo.lock
// 用法：node scripts/bump-version.mjs 0.3.1   （或 npm run bump -- 0.3.1）
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];

if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("用法：node scripts/bump-version.mjs <x.y.z>（如 0.3.1）");
  process.exit(1);
}

// 小工具：校验字段存在后替换；无变化时如实报告
function patch(rel, label, re, replacement) {
  const path = resolve(root, rel);
  const before = readFileSync(path, "utf8");
  const m = before.match(re);
  if (!m) {
    console.error(`✗ ${label}：未找到版本号字段，请检查文件格式`);
    process.exit(1);
  }
  const after = before.replace(re, replacement);
  if (after === before) {
    console.log(`= ${label} 已是 ${version}，无需改动`);
  } else {
    writeFileSync(path, after);
    console.log(`✓ ${label} ${m[1]} → ${version}`);
  }
}

patch("package.json", "package.json", /"version":\s*"([^"]+)"/, `"version": "${version}"`);
patch("src-tauri/tauri.conf.json", "tauri.conf.json", /"version":\s*"([^"]+)"/, `"version": "${version}"`);
patch("src-tauri/Cargo.toml", "Cargo.toml", /^version\s*=\s*"([^"]+)"/m, `version = "${version}"`);
// Cargo.lock 里只改本项目自己的版本条目（依赖的不动）；\r?\n 兼容 CRLF
const lockRe =
  /(\[\[package\]\]\r?\nname\s*=\s*"kimicodebar"\r?\nversion\s*=\s*")([^"]+)(")/;
const lockPath = resolve(root, "src-tauri/Cargo.lock");
const lockBefore = readFileSync(lockPath, "utf8");
const lockM = lockBefore.match(lockRe);
if (!lockM) {
  console.error("✗ Cargo.lock：未找到 kimicodebar 版本条目");
  process.exit(1);
}
if (lockM[2] === version) {
  console.log(`= Cargo.lock 已是 ${version}，无需改动`);
} else {
  writeFileSync(lockPath, lockBefore.replace(lockRe, `$1${version}$3`));
  console.log(`✓ Cargo.lock ${lockM[2]} → ${version}`);
}

console.log(`\n版本号已全部同步为 ${version}，接下来：`);
console.log(`  git add -A && git commit -m "chore: bump version to ${version}"`);
console.log(`  git push origin main && git tag v${version} && git push origin v${version}`);
