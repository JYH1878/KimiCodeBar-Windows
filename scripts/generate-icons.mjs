// 零依赖图标生成脚本：手拼 PNG chunk（zlib.deflateSync + 自算 CRC32），
// ICO 采用 PNG-compressed entries（Windows Vista+ 支持）。
// 用法：node scripts/generate-icons.mjs
// 产物输出到 src-tauri/icons/。
//
// 设计："月牙 + 环形进度弧"，透明背景，4x 超采样抗锯齿。
// 以 256x256 为设计空间，其他尺寸等比缩放：
//   - 月牙盘：圆心 (118,138)，半径 58；阴影圆圆心向其右上偏移 26px、
//     半径 50，做减法得到开口朝右上的月牙。
//   - 进度弧：圆心 (128,128)，半径 88，线宽 14，圆头端点，扫掠 270°，
//     90° 缺口居中于右上 45° 方向，与月牙开口呼应。
// 配色：正常版 #7aa2f7 -> #bb9af7（月牙沿对角线、弧沿角度渐变），
//       告警版 #f7768e -> #ff9e64，构图相同。

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

// ---------- 设计参数（256x256 设计空间）----------
const DESIGN = 256;
const MOON = { cx: 118, cy: 138, r: 58 }; // 月牙盘
const MOON_CUT_OFFSET = 26; // 阴影圆相对月牙盘向右上的偏移距离
const MOON_CUT = (() => {
  const d = MOON_CUT_OFFSET / Math.SQRT2; // 沿 (1,-1) 方向分解
  return { cx: MOON.cx + d, cy: MOON.cy - d, r: 50 };
})();
const ARC = { cx: 128, cy: 128, r: 88, width: 14 };
const ARC_GAP_CENTER = -45; // 缺口中心方向：右上 45°（屏幕坐标，度）
const ARC_SWEEP = 270;
// 弧起止角（屏幕坐标：+x 向右，+y 向下，顺时针为正）
const ARC_START = ((ARC_GAP_CENTER + (360 - ARC_SWEEP) / 2) + 360) % 360; // 0°
const ARC_END = (ARC_START + ARC_SWEEP) % 360; // 270°

const PALETTES = {
  normal: ['#7aa2f7', '#bb9af7'], // 蓝 -> 紫
  warn: ['#f7768e', '#ff9e64'], // 红 -> 橙
};

function hexToRgb(hex) {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function lerpRgb(c0, c1, t) {
  return [lerp(c0[0], c1[0], t), lerp(c0[1], c1[1], t), lerp(c0[2], c1[2], t)];
}

// ---------- 渲染 ----------
const SS = 4; // 超采样倍率

// 在 size*SS 分辨率下逐采样点判定覆盖并上色（预乘 alpha 累积），
// 再盒式滤波降采样到 size x size。
function renderIcon(size, palette) {
  const S = size * SS;
  const scale = S / DESIGN; // 设计空间 -> 渲染像素
  const [cA, cB] = palette.map(hexToRgb);
  // 预乘累积缓冲：r*a, g*a, b*a, a
  const acc = new Float64Array(S * S * 4);

  // 月牙渐变方向：左下 -> 右上（单位向量）
  const gdx = Math.SQRT1_2;
  const gdy = -Math.SQRT1_2;

  // 弧端点（圆头帽中心），设计空间
  const rad = (deg) => (deg * Math.PI) / 180;
  const cap0 = {
    x: ARC.cx + ARC.r * Math.cos(rad(ARC_START)),
    y: ARC.cy + ARC.r * Math.sin(rad(ARC_START)),
  };
  const cap1 = {
    x: ARC.cx + ARC.r * Math.cos(rad(ARC_END)),
    y: ARC.cy + ARC.r * Math.sin(rad(ARC_END)),
  };
  const halfW = ARC.width / 2;

  for (let y = 0; y < S; y++) {
    const py = (y + 0.5) / scale; // 设计空间坐标
    for (let x = 0; x < S; x++) {
      const px = (x + 0.5) / scale;

      // --- 月牙：disc 减去右上阴影圆 ---
      const mdx = px - MOON.cx;
      const mdy = py - MOON.cy;
      const inDisc = mdx * mdx + mdy * mdy <= MOON.r * MOON.r;
      let rgb = null;
      if (inDisc) {
        const cdx = px - MOON_CUT.cx;
        const cdy = py - MOON_CUT.cy;
        const inCut = cdx * cdx + cdy * cdy <= MOON_CUT.r * MOON_CUT.r;
        if (!inCut) {
          // 对角线渐变：投影到 (1,-1)/√2 方向，范围 [-r, r] -> [0, 1]
          const proj = mdx * gdx + mdy * gdy;
          const t = Math.min(1, Math.max(0, 0.5 + proj / (2 * MOON.r)));
          rgb = lerpRgb(cA, cB, t);
        }
      }

      // --- 进度弧 ---
      if (!rgb) {
        const adx = px - ARC.cx;
        const ady = py - ARC.cy;
        const d = Math.sqrt(adx * adx + ady * ady);
        // 点的角度（屏幕坐标，0..360，顺时针）
        const ang = (Math.atan2(ady, adx) * 180) / Math.PI;
        const ang360 = (ang + 360) % 360;
        // 相对弧起点的顺时针扫掠量
        const sweepPos = (ang360 - ARC_START + 360) % 360;
        let dist;
        if (sweepPos <= ARC_SWEEP) {
          dist = Math.abs(d - ARC.r);
        } else {
          // 缺口区域：看是否落在圆头帽内
          const d0 = Math.hypot(px - cap0.x, py - cap0.y);
          const d1 = Math.hypot(px - cap1.x, py - cap1.y);
          dist = Math.min(d0, d1);
        }
        if (dist <= halfW) {
          // 角度方向渐变：沿扫掠方向 0 -> 1
          const t = Math.min(1, Math.max(0, sweepPos / ARC_SWEEP));
          rgb = lerpRgb(cA, cB, t);
        }
      }

      if (rgb) {
        const i = (y * S + x) * 4;
        acc[i] = rgb[0];
        acc[i + 1] = rgb[1];
        acc[i + 2] = rgb[2];
        acc[i + 3] = 1;
      }
    }
  }

  // 盒式滤波降采样（SS x SS -> 1），预乘平均后反预乘
  const out = Buffer.alloc(size * size * 4);
  const n = SS * SS;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let sr = 0, sg = 0, sb = 0, sa = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const i = ((y * SS + sy) * S + (x * SS + sx)) * 4;
          const a = acc[i + 3];
          sr += acc[i] * a;
          sg += acc[i + 1] * a;
          sb += acc[i + 2] * a;
          sa += a;
        }
      }
      const o = (y * size + x) * 4;
      if (sa === 0) continue; // 全透明
      const alpha = sa / n;
      out[o] = Math.round(sr / sa);
      out[o + 1] = Math.round(sg / sa);
      out[o + 2] = Math.round(sb / sa);
      out[o + 3] = Math.round(alpha * 255);
    }
  }
  return out;
}

function iconPNG(size, palette) {
  return encodePNG(size, size, renderIcon(size, palette));
}

// ---------- 生成 ----------
fs.mkdirSync(ICONS_DIR, { recursive: true });

const outputs = [
  ['tray-normal.png', iconPNG(32, PALETTES.normal)],
  ['tray-warn.png', iconPNG(32, PALETTES.warn)],
  ['icon.png', iconPNG(256, PALETTES.normal)],
  ['32x32.png', iconPNG(32, PALETTES.normal)],
  ['128x128.png', iconPNG(128, PALETTES.normal)],
  ['128x128@2x.png', iconPNG(256, PALETTES.normal)],
];

for (const [name, buf] of outputs) {
  fs.writeFileSync(path.join(ICONS_DIR, name), buf);
  console.log(`written ${name} (${buf.length} bytes)`);
}

// icon.ico：16/32/48，PNG-compressed entries
const ico = buildICO([
  { size: 16, buf: iconPNG(16, PALETTES.normal) },
  { size: 32, buf: iconPNG(32, PALETTES.normal) },
  { size: 48, buf: iconPNG(48, PALETTES.normal) },
]);
fs.writeFileSync(path.join(ICONS_DIR, 'icon.ico'), ico);
console.log(`written icon.ico (${ico.length} bytes, entries: 16/32/48)`);
