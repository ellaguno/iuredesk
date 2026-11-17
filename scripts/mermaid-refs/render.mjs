// S1.21: dibuja cada diagrama del corpus (crates/html-view/tests/mermaid/*.mmd) con mermaid.js en
// un Chromium sin pantalla y guarda el PNG en crates/html-view/tests/mermaid/ref/. Sirve para
// comparar a ojo con lo que dibuja mermaid-svg (`cargo run -p iuredesk-html-view --example
// mermaid_corpus -- <carpeta> --compare`). Solo desarrollo: el producto no depende de esto.
//
//   cd scripts/mermaid-refs && npm ci && npm run refs
import { readdir, readFile, mkdir } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer';

const here = path.dirname(fileURLToPath(import.meta.url));
const corpus = path.resolve(here, '../../crates/html-view/tests/mermaid');
const out = path.join(corpus, 'ref');
const require = createRequire(import.meta.url);
const mermaidJs = require.resolve('mermaid/dist/mermaid.min.js');

await mkdir(out, { recursive: true });
const browser = await puppeteer.launch();
try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1400, height: 900, deviceScaleFactor: 1.5 });
  await page.setContent('<!doctype html><html><body style="margin:0;background:#fff;font-family:sans-serif"><div id="d"></div></body></html>');
  await page.addScriptTag({ path: mermaidJs });
  await page.evaluate(() => window.mermaid.initialize({ startOnLoad: false, theme: 'default', fontSize: 14 }));
  const files = (await readdir(corpus)).filter((f) => f.endsWith('.mmd')).sort();
  for (const f of files) {
    const code = await readFile(path.join(corpus, f), 'utf8');
    const stem = f.replace(/\.mmd$/, '');
    try {
      await page.evaluate(async (code) => {
        const { svg } = await window.mermaid.render('ref', code);
        document.getElementById('d').innerHTML = svg;
      }, code);
      const el = await page.$('#d svg');
      await el.screenshot({ path: path.join(out, `${stem}.png`), omitBackground: false });
      console.log(`ok   ${stem}`);
    } catch (e) {
      console.log(`FALLA ${stem}: ${e.message ?? e}`);
    }
  }
} finally {
  await browser.close();
}
