# Diagnóstico: de que jeito o AudioWorklet carrega em cada origem (blob: e data:).
# Uso: python3 test/wl.py file:///caminho/dist/tonica.html http://localhost:8000/tonica.html
from playwright.sync_api import sync_playwright
import sys, os
CHROME = os.environ.get('CHROME') or ('/usr/bin/google-chrome' if os.path.exists('/usr/bin/google-chrome') else None)
JS ="""async () => {
  const out = {};
  const code = 'registerProcessor("x' + Math.random().toString(36).slice(2) + '", class extends AudioWorkletProcessor{process(){return true}})';
  const ctx = new AudioContext();
  out.origin = location.origin; out.secure = isSecureContext; out.hasWorklet = !!ctx.audioWorklet;
  try { await ctx.audioWorklet.addModule(URL.createObjectURL(new Blob([code], { type: 'application/javascript' }))); out.blob = 'ok'; } catch (e) { out.blob = e.name + ': ' + e.message; }
  try { await ctx.audioWorklet.addModule('data:application/javascript;base64,' + btoa(code.replace('"x', '"y'))); out.data = 'ok'; } catch (e) { out.data = e.name + ': ' + e.message; }
  return out;
}"""
with sync_playwright() as p:
    b = p.chromium.launch(executable_path=CHROME, headless=True, args=['--autoplay-policy=no-user-gesture-required'])
    pg = b.new_page()
    for url in sys.argv[1:]:
        pg.goto(url, wait_until='domcontentloaded'); print(url[:40], pg.evaluate(JS))
    b.close()
