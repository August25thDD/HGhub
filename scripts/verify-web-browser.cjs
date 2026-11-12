// Live browser QA. Never logs passwords, original media URLs or response bodies.
const { readFileSync, writeFileSync, mkdirSync } = require('node:fs');
const { chromium, webkit, devices } = require(process.env.HGHUB_PLAYWRIGHT_MODULE || 'playwright-core');
const origin = process.env.HGHUB_TEST_ORIGIN || 'http://127.0.0.1:8900';
const password = readFileSync('.clawbench/private/web-password.txt', 'utf8').trim();
const report = [];

(async () => {
  for (const [name, engine, options] of [
    ['desktop-chromium', chromium, { viewport: { width: 1440, height: 900 } }],
    ['iphone-webkit', webkit, { ...devices['iPhone 13'] }],
  ].filter(([name]) => !process.env.HGHUB_BROWSER_CASES || process.env.HGHUB_BROWSER_CASES.split(',').includes(name))) {
    let browser;
    const result = { name, passed: false, pageErrors: [], proxy: { images: 0, playlists: 0, segments: 0 } };
    try {
      browser = await engine.launch({ headless: true });
      const context = await browser.newContext(options);
      const page = await context.newPage();
      page.on('pageerror', error => result.pageErrors.push(error.name));
      const statuses = [];
      page.on('response', response => {
        const path = new URL(response.url()).pathname;
        if (path.startsWith('/media/') && response.status() === 200) {
          if (path === '/media/img') result.proxy.images++;
          if (path === '/media/hls') result.proxy.playlists++;
          if (path === '/media/seg') result.proxy.segments++;
        }
        if (path === '/api/invoke/api_feed' || path === '/api/invoke/api_video') statuses.push({ path, status: response.status() });
      });
      await page.goto(origin);
      await page.waitForURL('**/login');
      await page.getByLabel('账号', { exact: true }).fill(process.env.HGHUB_TEST_USERNAME || 'hghub');
      await page.getByLabel('访问密码').fill(password);
      await page.getByRole('button', { name: '登录', exact: true }).click();
      await page.waitForURL(origin + '/');
      await page.waitForFunction(() => document.querySelector('video')?.getAttribute('src'), undefined, { timeout: 60000 });
      await page.waitForFunction(() => [...document.querySelectorAll('video')].some(v => v.videoWidth > 0 && v.readyState >= 2), undefined, { timeout: 60000 });
      result.video = await page.locator('video').first().evaluate(v => ({ width: v.videoWidth, height: v.videoHeight, readyState: v.readyState, duration: v.duration, currentTime: v.currentTime }));
      result.layout = await page.evaluate(() => ({ viewport: innerWidth, scrollWidth: document.documentElement.scrollWidth }));
      if (result.layout.scrollWidth > result.layout.viewport) throw new Error('horizontal overflow');
      if (!statuses.some(s => s.path.endsWith('api_feed') && s.status === 200) || !statuses.some(s => s.path.endsWith('api_video') && s.status === 200)) throw new Error('feed/detail response missing');
      if (!result.proxy.images || !result.proxy.playlists || !result.proxy.segments) throw new Error('proxy chain incomplete');
      if (result.pageErrors.length) throw new Error('uncaught browser errors');
      result.passed = true;
    } catch (error) {
      result.error = error.name + ': ' + String(error.message).replace(/https?:\/\/[^\s]+/g, '[url]').slice(0, 500);
      process.exitCode = 1;
    } finally {
      if (browser) await browser.close();
      report.push(result);
      mkdirSync('.clawbench/reports', { recursive: true });
      writeFileSync(process.env.HGHUB_BROWSER_REPORT_FILE || '.clawbench/reports/web-browser.json', JSON.stringify(report, null, 2) + '\n');
      console.log(JSON.stringify(result));
    }
  }
})();
