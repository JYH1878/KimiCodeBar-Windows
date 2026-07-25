// 零依赖图标生成脚本：手拼 PNG chunk（zlib.deflateSync + 自算 CRC32），
// ICO 采用 PNG-compressed entries（Windows Vista+ 支持）。
// 用法：node scripts/generate-icons.mjs
// 产物输出到 src-tauri/icons/。

import zlib from 'node:zlib';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ICONS_DIR = path.join(__dirname, '..', 'src-tauri', 'icons');

// ---------- CRC32 ----------
const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    crc = CRC_TABLE[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

// ---------- PNG 编码 ----------
function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

function encodePNG(width, height, rgba) {
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type: RGBA
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  const stride = width * 4 + 1;
  const raw = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    raw[y * stride] = 0; // filter: none
    rgba.copy(raw, y * stride + 1, y * width * 4, (y + 1) * width * 4);
  }
  const idat = zlib.deflateSync(raw, { level: 9 });
  return Buffer.concat([
    sig,
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', idat),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------- ICO 容器（PNG-compressed entries）----------
function buildICO(images) {
  // images: [{ size, buf }]
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(images.length, 4);
  let offset = 6 + 16 * images.length;
  const entries = [];
  for (const { size, buf } of images) {
    const e = Buffer.alloc(16);
    e[0] = size >= 256 ? 0 : size; // 0 表示 256
    e[1] = size >= 256 ? 0 : size;
    e[2] = 0; // palette
    e[3] = 0; // reserved
    e.writeUInt16LE(1, 4); // planes
    e.writeUInt16LE(32, 6); // bit count
    e.writeUInt32LE(buf.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += buf.length;
    entries.push(e);
  }
  return Buffer.concat([header, ...entries, ...images.map((i) => i.buf)]);
}

// ---------- 圆形图标绘制 ----------
function hexToRgb(hex) {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function lighten([r, g, b], t) {
  return [r + (255 - r) * t, g + (255 - g) * t, b + (255 - b) * t].map(Math.round);
}

function darken([r, g, b], t) {
  return [r * (1 - t), g * (1 - t), b * (1 - t)].map(Math.round);
}

// 透明背景 + 垂直渐变实心圆，边缘 1px 抗锯齿
function renderCircle(size, baseHex) {
  const top = lighten(hexToRgb(baseHex), 0.18);
  const bottom = darken(hexToRgb(baseHex), 0.16);
  const buf = Buffer.alloc(size * size * 4);
  const cx = size / 2;
  const cy = size / 2;
  const r = size / 2 - Math.max(1, size / 16); // 留出透明边距
  for (let y = 0; y < size; y++) {
    const t = (y + 0.5) / size;
    const cr = Math.round(top[0] + (bottom[0] - top[0]) * t);
    const cg = Math.round(top[1] + (bottom[1] - top[1]) * t);
    const cb = Math.round(top[2] + (bottom[2] - top[2]) * t);
    for (let x = 0; x < size; x++) {
      const dx = x + 0.5 - cx;
      const dy = y + 0.5 - cy;
      const dist = Math.sqrt(dx * dx + dy * dy);
      const alpha = Math.max(0, Math.min(1, r + 0.5 - dist));
      if (alpha <= 0) continue;
      const i = (y * size + x) * 4;
      buf[i] = cr;
      buf[i + 1] = cg;
      buf[i + 2] = cb;
      buf[i + 3] = Math.round(alpha * 255);
    }
  }
  return buf;
}

function circlePNG(size, baseHex) {
  return encodePNG(size, size, renderCircle(size, baseHex));
}

// ---------- 生成 ----------
const NORMAL = '#6b7cff'; // 蓝灰：正常状态
const WARN = '#e53e3e'; // 红色：告警状态

fs.mkdirSync(ICONS_DIR, { recursive: true });

const outputs = [
  ['tray-normal.png', circlePNG(32, NORMAL)],
  ['tray-warn.png', circlePNG(32, WARN)],
  ['icon.png', circlePNG(256, NORMAL)],
  ['32x32.png', circlePNG(32, NORMAL)],
  ['128x128.png', circlePNG(128, NORMAL)],
  ['128x128@2x.png', circlePNG(256, NORMAL)],
];

for (const [name, buf] of outputs) {
  fs.writeFileSync(path.join(ICONS_DIR, name), buf);
  console.log(`written ${name} (${buf.length} bytes)`);
}

// icon.ico：16/32/48，PNG-compressed entries
const ico = buildICO([
  { size: 16, buf: circlePNG(16, NORMAL) },
  { size: 32, buf: circlePNG(32, NORMAL) },
  { size: 48, buf: circlePNG(48, NORMAL) },
]);
fs.writeFileSync(path.join(ICONS_DIR, 'icon.ico'), ico);
console.log(`written icon.ico (${ico.length} bytes, entries: 16/32/48)`);
