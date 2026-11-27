#!/usr/bin/env node
/**
 * Capture the README hero screenshot of index.html.
 *
 * Crop rules:
 *   - Width ≈ chart width + some extra padding, centered on the chart
 *   - Height from the page title through the 3rd table body row
 *   - GitHub fork ribbon excluded (hidden before capture)
 *
 * Usage (from repo root):
 *   ./scripts/capture_readme_screenshot.sh
 *   # or, with playwright available:
 *   node scripts/capture_readme_screenshot.mjs [output-path]
 *
 * Defaults to docs/screenshot.png.
 * 
 * DISCLAIMER: vibe coded slop file.
 */

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, '..');
const DEFAULT_OUT = path.join(REPO_ROOT, 'docs', 'screenshot.png');

/** ~1 cm at CSS 96 dpi — total extra width beyond the chart. */
const CM_PX = 40;
// Tall enough that title → 3rd table row fits without scrolling; clip crops the rest.
const VIEWPORT = { width: 1280, height: 1600 };
const DEVICE_SCALE_FACTOR = 2;

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.jsonl': 'application/jsonl',
  '.png': 'image/png',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
};

function startStaticServer(root) {
  const server = http.createServer((req, res) => {
    const urlPath = decodeURIComponent((req.url || '/').split('?')[0]);
    const rel = urlPath === '/' ? '/index.html' : urlPath;
    const filePath = path.normalize(path.join(root, rel));
    if (!filePath.startsWith(root + path.sep) && filePath !== root) {
      res.writeHead(403).end('Forbidden');
      return;
    }
    fs.readFile(filePath, (err, data) => {
      if (err) {
        res.writeHead(err.code === 'ENOENT' ? 404 : 500).end(String(err.message));
        return;
      }
      const ext = path.extname(filePath).toLowerCase();
      res.writeHead(200, { 'Content-Type': MIME[ext] || 'application/octet-stream' });
      res.end(data);
    });
  });
  return new Promise((resolve, reject) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      resolve({ server, port });
    });
    server.on('error', reject);
  });
}

async function waitForAppReady(page) {
  await page.waitForFunction(() => {
    const overlay = document.getElementById('overlay');
    const rows = document.querySelectorAll('#tableBody tr');
    const chart = document.getElementById('chart');
    const overlayHidden = overlay?.classList.contains('hidden');
    const hasCanvas = !!chart?.querySelector('canvas');
    return overlayHidden && hasCanvas && rows.length >= 3;
  }, { timeout: 60_000 });
}

/**
 * Clip rectangle in CSS pixels: chart-centered width, title → 3rd table row.
 */
async function computeClip(page) {
  return page.evaluate((cmPx) => {
    const chart = document.getElementById('chart');
    const title = document.querySelector('.page-title');
    const rows = document.querySelectorAll('#tableBody tr');
    if (!chart || !title || rows.length < 3) {
      throw new Error('Expected chart, title, and ≥3 table rows');
    }
    const third = rows[2];
    const chartRect = chart.getBoundingClientRect();
    const titleRect = title.getBoundingClientRect();
    const thirdRect = third.getBoundingClientRect();

    const sidePad = cmPx / 2;
    const left = Math.max(0, chartRect.left - sidePad);
    const right = chartRect.right + sidePad;
    const top = Math.max(0, titleRect.top);
    // Small bottom pad so the 3rd row isn’t flush against the edge.
    const bottom = thirdRect.bottom + 8;

    return {
      x: Math.round(left),
      y: Math.round(top),
      width: Math.round(right - left),
      height: Math.round(bottom - top),
    };
  }, CM_PX);
}

async function main() {
  const outPath = path.resolve(process.argv[2] || DEFAULT_OUT);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });

  const { server, port } = await startStaticServer(REPO_ROOT);
  const url = `http://127.0.0.1:${port}/index.html`;

  let browser;
  try {
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage({
      viewport: VIEWPORT,
      deviceScaleFactor: DEVICE_SCALE_FACTOR,
    });

    await page.goto(url, { waitUntil: 'networkidle' });
    await waitForAppReady(page);

    // Exclude the corner ribbon from the capture (and from layout noise).
    await page.locator('.github-corner').evaluate((el) => {
      el.style.display = 'none';
    });

    const clip = await computeClip(page);
    await page.screenshot({
      path: outPath,
      type: 'png',
      clip,
      animations: 'disabled',
    });

    console.error(
      `[screenshot] wrote ${outPath} (${clip.width}×${clip.height} CSS px @ ${DEVICE_SCALE_FACTOR}x)`,
    );
  } finally {
    if (browser) await browser.close();
    server.close();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
