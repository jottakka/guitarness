# Segunda bateria de testes de navegador (violão virtual nos três modos, rodada de 60 s, paisagem, latência).
# Mesmos requisitos e variáveis de test/e2e.py.
import json, os, sys, pathlib
from playwright.sync_api import sync_playwright
RAIZ = pathlib.Path(__file__).resolve().parent.parent
URL = os.environ.get('URL', (RAIZ / 'dist' / 'tonica.html').as_uri())
W = str(RAIZ / 'test' / 'wav') + os.sep
S = str(RAIZ / 'shots') + os.sep
CHROME = os.environ.get('CHROME') or ('/usr/bin/google-chrome' if os.path.exists('/usr/bin/google-chrome') else None)
os.makedirs(S, exist_ok=True)
if not os.path.exists(W + 'soltas.wav'): sys.exit('faltam os WAVs de teste: rode  python3 test/gen_wav.py test/wav')
logs = []
def novo(p, vp=(390, 844), fake=None):
    args = ['--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream']
    if fake: args.append('--use-file-for-fake-audio-capture=' + fake)
    b = p.chromium.launch(executable_path=CHROME, headless=True, args=args)
    pg = b.new_context(viewport={'width': vp[0], 'height': vp[1]}, device_scale_factor=2).new_page()
    pg.on('console', lambda m: logs.append((m.type, m.text)) if m.type in ('error', 'warning') else None)
    pg.on('pageerror', lambda e: logs.append(('pageerror', str(e))))
    pg.goto(URL, wait_until='domcontentloaded'); pg.wait_for_timeout(600); return b, pg
T = lambda pg, s: pg.locator(s).inner_text()
with sync_playwright() as p:
    b, pg = novo(p)
    pg.evaluate("tonica.Som.ligarVirtual()"); pg.wait_for_timeout(300)
    print('barra:', T(pg, '#oModo'))
    # 1) Monta-Acorde guiado com arpejo: notas do desenho soando juntas (sem abafar)
    pg.click('.modo[data-ir=chord]'); pg.click('#chGo'); pg.wait_for_timeout(300)
    r = pg.evaluate("""async () => {
      const sleep = ms => new Promise(r => setTimeout(r, ms)), out = [];
      for (let k = 0; k < 6; k++) {
        const ms = [...document.querySelectorAll('#chBraco .ponto')].map(e => +e.getAttribute('data-midi')).sort((a, b) => a - b);
        const t0 = performance.now(); let n = 0;
        for (const m of ms) { tonica.Som.virtual(m, 0.2); n++; await sleep(380); if (!document.querySelector('#cProx').hidden) break; }
        await sleep(250);
        out.push({ acorde: document.querySelector('#cExt').textContent, cordas: n + '/' + ms.length, pronto: !document.querySelector('#cProx').hidden, vagas: document.querySelectorAll('#cSlots .slot.ok').length });
        if (document.querySelector('#cProx').hidden) break;
        await sleep(1800);                       // deixa o acorde morrer antes do próximo
        document.querySelector('#cProx').click(); await sleep(300);
      }
      return out;
    }""")
    for x in r: print('  arpejo', x)
    print('  fim:', T(pg, '#fTitulo') if pg.locator('#t-fim.on').count() else '(não chegou ao fim)', '|', T(pg, '#fStats').replace('\n', ' ') if pg.locator('#t-fim.on').count() else '')
    # 2) Caça-Notas estilo Notas (qualquer oitava) com violão virtual
    pg.click('#btnHome'); pg.click('.modo[data-ir=hunt]'); pg.locator('#huntFases .chip').nth(1).click(); pg.click('#huntEstilo button[data-v=notas]'); pg.click('#huntGo'); pg.wait_for_timeout(2600)
    r = pg.evaluate("""async () => {
      const sleep = ms => new Promise(r => setTimeout(r, ms)), PT = ['Dó','Dó#','Ré','Ré#','Mi','Fá','Fá#','Sol','Sol#','Lá','Lá#','Si'], out = { ok: 0, n: 0, dts: [] };
      for (let i = 0; i < 12; i++) {
        await sleep(450);
        const nome = document.querySelector('#hNota').textContent, pc = PT.indexOf(nome); if (pc < 0) continue;
        const antes = document.querySelector('#hPontos').textContent, midi = 52 + ((pc - 4 + 12) % 12);   // uma oitava qualquer
        const t0 = performance.now(); tonica.Som.virtual(midi, 0.2); out.n++;
        let dt = -1; while (performance.now() - t0 < 1200) { await sleep(4); if (document.querySelector('#hPontos').textContent !== antes) { dt = performance.now() - t0; break; } }
        if (dt > 0) { out.ok++; out.dts.push(Math.round(dt)); }
      }
      return out;
    }""")
    print('estilo Notas (virtual):', r['ok'], 'de', r['n'], '| ms:', sorted(r['dts']))
    # 3) deixa a rodada acabar sozinha (60 s)
    pg.wait_for_selector('#t-fim.on', timeout=70000)
    print('fim natural:', T(pg, '#fOlho'), '|', T(pg, '#fPontos'), '|', T(pg, '#fMsg'))
    # 4) Jam com violão virtual: notas da escala e uma de fora
    pg.click('#fInicio'); pg.click('.modo[data-ir=jam]'); pg.click('#jamGo'); pg.wait_for_timeout(1000)
    pg.evaluate("""async () => { const sleep = ms => new Promise(r => setTimeout(r, ms)); for (const m of [57, 60, 62, 64, 67, 69, 58, 64, 60, 57]) { tonica.Som.virtual(m, 0.2); await sleep(420); } }""")
    print('jam virtual: dentro', T(pg, '#jDentro'), '| pontos', T(pg, '#jPontos'), '| fala:', T(pg, '#jFala'))
    pg.screenshot(path=S + '33-jam-virtual.png')
    pg.click('#jamSair'); pg.wait_for_timeout(200); print('   ', T(pg, '#fStats').replace('\n', ' '))
    b.close()
    # 5) paisagem no celular
    b, pg = novo(p, vp=(844, 390)); pg.click('.modo[data-ir=hunt]'); pg.click('#huntGo'); pg.wait_for_timeout(2700); pg.screenshot(path=S + '42-paisagem.png'); b.close()
    # 6) medição de latência sem caminho acústico: precisa falhar com mensagem clara
    b, pg = novo(p, fake=W + 'soltas.wav'); pg.click('#micBtn'); pg.wait_for_timeout(500); pg.click('.modo[data-ir=afina]'); pg.click('#aLat'); pg.wait_for_timeout(7500)
    print('latência (sem alto-falante ligado ao mic):', T(pg, '#aLatOut')); b.close()
print('console:', logs[:8] if logs else 'sem erros')
