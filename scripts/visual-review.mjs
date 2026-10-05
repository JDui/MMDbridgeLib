import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Test-only bridge and browser. No production database, source files, or desktop window.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { build } = require(join(root, 'apps/desktop/node_modules/esbuild'));
const { chromium } = require(process.env.MBL_PLAYWRIGHT_MODULE ?? 'playwright');
const output = process.env.MBL_REVIEW_OUTPUT ?? await fs.mkdtemp(join(os.tmpdir(), 'mbl-visual-review-'));
await fs.mkdir(output, { recursive: true });
await build({ entryPoints: [join(root, 'apps/desktop/src/main.tsx')], bundle: true, format: 'iife',
  outfile: join(output, 'review.js'), minify: true, logLevel: 'silent' });
const [js, css, bridge] = await Promise.all([
  fs.readFile(join(output, 'review.js'), 'utf8'), fs.readFile(join(output, 'review.css'), 'utf8'),
  fs.readFile(join(root, 'scripts/visual-review-fixture.js'), 'utf8'),
]);
const html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>MBL</title><style id="library-style-nonce"></style><style>${css}</style></head><body><div id="root"></div><script>${bridge}</script><script>${js.replaceAll('</script', '<\\/script')}</script></body></html>`;
const server = http.createServer((_request, response) => {
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }); response.end(html);
});
await new Promise((ready, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', ready); });
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1480, height: 960 }, colorScheme: 'dark' });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const url = `http://127.0.0.1:${server.address().port}`;
  await page.goto(`${url}/?review=recovery&scheme=dark`);
  await page.getByRole('button', { name: '操作日志', exact: true }).click();
  let journal = page.getByRole('dialog', { name: '资产操作日志', exact: true });
  await journal.getByText('需要检查', { exact: true }).waitFor();
  const details = await journal.locator('details').all();
  assert.equal(details.length, 3);
  for (const detail of details) await detail.locator('summary').click();
  const checkEvidence = async () => {
    assert.equal(await journal.locator('details').count(), 3);
    for (const name of ['初音ミク', '洛天依', '乐正绫']) {
      assert.equal(await journal.getByText(`E:\\MMD\\Models\\${name}`, { exact: true }).last().isVisible(), true);
    }
  };
  await checkEvidence();
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: join(output, 'MBL_Continue_Stage5_Recovery.jpg'), type: 'jpeg', quality: 92, animations: 'disabled' });
  await journal.getByRole('button', { name: '已人工恢复并重扫，标记已核对', exact: true }).click();
  await page.getByRole('dialog', { name: '确认操作', exact: true }).getByRole('button', { name: '确认', exact: true }).click();
  await journal.getByText('已核对', { exact: true }).waitFor();
  assert.equal(await journal.getByRole('button', { name: '已人工恢复并重扫，标记已核对', exact: true }).count(), 0);
  await checkEvidence();
  await page.screenshot({ path: join(output, 'MBL_Continue_Stage6_Resolved.jpg'), type: 'jpeg', quality: 92, animations: 'disabled' });
  await page.goto(`${url}/?review=live&scheme=dark`);
  await page.getByRole('button', { name: '操作日志', exact: true }).click();
  journal = page.getByRole('dialog', { name: '资产操作日志', exact: true });
  await journal.getByText('进行中', { exact: true }).waitFor();
  assert.equal(await journal.getByRole('button', { name: '已人工恢复并重扫，标记已核对', exact: true }).count(), 0);
  await page.screenshot({ path: join(output, 'MBL_Continue_Stage7_Active.jpg'), type: 'jpeg', quality: 92, animations: 'disabled' });
  assert.deepEqual(errors, []);
  await fs.writeFile(join(output, 'verification.json'), JSON.stringify({ sourceCommit: process.env.GITHUB_SHA,
    sampleData: true, headless: true, recoveryEvidenceGroups: 3, resolvedEvidenceGroups: 3,
    activeResolveButtonCount: 0, pageErrors: errors }, null, 2));
  console.log('Recovery evidence, resolved evidence, and active-operation controls verified.');
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
