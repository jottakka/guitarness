# Testes de navegador da demo. Uso: python3 test/e2e.py [toque|virtual|arquivo|mic|desktop]
# Precisa de: pip install playwright; WAVs em test/wav (python3 test/gen_wav.py test/wav).
# Variáveis: URL (padrão: dist/tonica.html por file://) e CHROME (executável; padrão: o Chromium do Playwright).
import json, sys, time, os, pathlib
from playwright.sync_api import sync_playwright
RAIZ = pathlib.Path(__file__).resolve().parent.parent
URL = os.environ.get('URL', (RAIZ / 'dist' / 'tonica.html').as_uri())
W = str(RAIZ / 'test' / 'wav') + os.sep
S = str(RAIZ / 'shots') + os.sep
CHROME = os.environ.get('CHROME') or ('/usr/bin/google-chrome' if os.path.exists('/usr/bin/google-chrome') else None)
os.makedirs(S, exist_ok=True)
if not os.path.exists(W + 'soltas.wav'): sys.exit('faltam os WAVs de teste: rode  python3 test/gen_wav.py test/wav')
logs = []
def novo(p, fake=None, vp=(390, 844)):
    args = ['--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream']
    if fake: args.append('--use-file-for-fake-audio-capture=' + fake)
    b = p.chromium.launch(executable_path=CHROME, headless=True, args=args)
    c = b.new_context(viewport={'width': vp[0], 'height': vp[1]}, device_scale_factor=2)
    pg = c.new_page()
    pg.on('console', lambda m: logs.append((m.type, m.text)) if m.type in ('error', 'warning') else None)
    pg.on('pageerror', lambda e: logs.append(('pageerror', str(e))))
    pg.goto(URL, wait_until='domcontentloaded'); pg.wait_for_timeout(700)
    return b, pg
def txt(pg, sel): return pg.locator(sel).inner_text()
def clica_alvo(pg, sel='#huntBraco .ponto.alvo'):
    bb = pg.locator(sel).first.bounding_box()
    pg.mouse.click(bb['x'] + bb['width'] / 2, bb['y'] + bb['height'] / 2)

which = sys.argv[1] if len(sys.argv) > 1 else 'all'
with sync_playwright() as p:
    if which in ('all', 'toque'):
        b, pg = novo(p)
        pg.screenshot(path=S + '01-home.png', full_page=True)
        # braço livre do início
        bb = pg.locator('#heroBraco .ponto').nth(3).bounding_box(); pg.mouse.click(bb['x'] + 8, bb['y'] + 8); pg.wait_for_timeout(120)
        print('home fala:', txt(pg, '#heroFala'))
        pg.screenshot(path=S + '02-home-toque.png')
        # ---- Caça-Notas guiado por toque
        pg.click('.modo[data-ir=hunt]'); pg.wait_for_timeout(200)
        pg.screenshot(path=S + '03-hunt-prep.png', full_page=True)
        pg.click('#huntGo'); pg.wait_for_timeout(2700)
        pg.screenshot(path=S + '04-hunt-jogo.png')
        for i in range(9):
            pg.wait_for_selector('#huntBraco .ponto.alvo', timeout=4000); pg.wait_for_timeout(150); clica_alvo(pg)
            if i == 4: pg.wait_for_timeout(60); pg.screenshot(path=S + '05-hunt-acerto.png')
            pg.wait_for_timeout(260)
        print('hunt guiado: pontos', txt(pg, '#hPontos'), 'seguidas', txt(pg, '#hCombo'), '| fala:', txt(pg, '#hFala'))
        # erro de propósito: toca uma bolinha que não é o alvo
        pg.wait_for_selector('#huntBraco .ponto.alvo'); alvo = pg.locator('#huntBraco .ponto.alvo').get_attribute('data-midi')
        outro = pg.locator('#huntBraco .ponto:not(.alvo):not(.trancado)').first; bb = outro.bounding_box(); pg.mouse.click(bb['x'] + bb['width'] / 2, bb['y'] + bb['height'] / 2); pg.wait_for_timeout(100)
        print('hunt erro  :', txt(pg, '#hFala'), '| seguidas', txt(pg, '#hCombo'))
        pg.screenshot(path=S + '06-hunt-erro.png')
        pg.click('#huntSair'); pg.wait_for_timeout(400)
        print('fim:', txt(pg, '#fOlho'), '|', txt(pg, '#fTitulo'), '|', txt(pg, '#fPontos'), '|', txt(pg, '#fStats').replace('\n', ' '))
        pg.screenshot(path=S + '07-fim.png', full_page=True)
        # ---- estilo Intervalos: espera estourar o tempo para ver a revelação
        pg.click('#fInicio'); pg.click('.modo[data-ir=hunt]'); pg.click('#huntEstilo button[data-v=intervalos]'); pg.locator('#huntFases .chip').nth(2).click(); pg.wait_for_timeout(150)
        pg.screenshot(path=S + '08-hunt-prep-intervalos.png', full_page=True)
        pg.click('#huntGo'); pg.wait_for_timeout(2800)
        print('intervalos pedido:', txt(pg, '#hNota'), '/', txt(pg, '#hSub'))
        pg.screenshot(path=S + '09-hunt-intervalos.png')
        pg.wait_for_selector('#huntBraco .ponto.alvo', timeout=9000)
        print('revelado  :', txt(pg, '#hFala'), '| alvos acesos:', pg.locator('#huntBraco .ponto.alvo').count())
        clica_alvo(pg); pg.wait_for_timeout(400)
        print('após revelado: pontos', txt(pg, '#hPontos'))
        pg.click('#btnHome')
        # ---- Monta-Acorde guiado
        pg.click('.modo[data-ir=chord]'); pg.wait_for_timeout(150); pg.screenshot(path=S + '10-chord-prep.png', full_page=True)
        pg.click('#chGo'); pg.wait_for_timeout(300); pg.screenshot(path=S + '11-chord-jogo.png')
        for k in range(6):
            n = pg.locator('#chBraco .ponto').count()
            for i in range(n):
                bb = pg.locator('#chBraco .ponto').nth(i).bounding_box(); pg.mouse.click(bb['x'] + bb['width'] / 2, bb['y'] + bb['height'] / 2); pg.wait_for_timeout(70)
                if pg.locator('#cProx').is_visible(): break
            if k == 0: pg.wait_for_timeout(150); pg.screenshot(path=S + '12-chord-pronto.png')
            print('  acorde', k + 1, txt(pg, '#cExt'), '| pontos', txt(pg, '#cPontos'), '|', txt(pg, '#cFala')[:60])
            pg.click('#cProx'); pg.wait_for_timeout(250)
        print('fim chord:', txt(pg, '#fTitulo'), txt(pg, '#fPontos'), '| extra:', txt(pg, '#fExtra'))
        pg.screenshot(path=S + '13-chord-fim.png', full_page=True)
        # ---- Pense rápido: erra duas vezes para ver a dica
        pg.click('#fInicio'); pg.click('.modo[data-ir=chord]'); pg.click('#chEstilo button[data-v=rapido]'); pg.click('#chGo'); pg.wait_for_timeout(300)
        pg.screenshot(path=S + '14-chord-rapido.png')
        # ---- Jam com a base
        pg.click('#btnHome'); pg.click('.modo[data-ir=jam]'); pg.wait_for_timeout(150); pg.screenshot(path=S + '15-jam-prep.png', full_page=True)
        pg.click('#jamGo'); pg.wait_for_timeout(1200)
        n = pg.locator('#jamBraco .ponto').count()
        for i in range(14):
            bb = pg.locator('#jamBraco .ponto').nth((i * 5) % n).bounding_box(); pg.mouse.click(bb['x'] + bb['width'] / 2, bb['y'] + bb['height'] / 2); pg.wait_for_timeout(170)
        pg.screenshot(path=S + '16-jam-jogo.png')
        print('jam: dentro', txt(pg, '#jDentro'), 'pontos', txt(pg, '#jPontos'), '| compasso on:', pg.locator('#jamFaixa .comp.on').count())
        pg.click('#jamSair'); pg.wait_for_timeout(300)
        print('fim jam:', txt(pg, '#fTitulo'), '|', txt(pg, '#fStats').replace('\n', ' '))
        # ---- Jam com arquivo: estima o tom
        pg.click('#fInicio'); pg.click('.modo[data-ir=jam]'); pg.click('#jamFonte button[data-v=musica]')
        pg.set_input_files('#jamArq', W + 'prog_em.wav'); pg.wait_for_function("document.querySelector('#jamArqInfo').textContent.includes('tom estimado') || document.querySelector('#jamArqInfo').textContent.includes('consegui')", timeout=20000)
        print('jam arquivo:', txt(pg, '#jamArqInfo'), '| seletor:', txt(pg, '#jamTomNome'))
        pg.screenshot(path=S + '17-jam-musica.png', full_page=True)
        pg.click('#jamGo'); pg.wait_for_timeout(900); pg.screenshot(path=S + '18-jam-musica-jogo.png'); pg.click('#jamSair'); pg.wait_for_timeout(200)
        # ---- plano e afinador (parado)
        pg.click('#fInicio'); pg.click('.modo[data-ir=plano]'); pg.wait_for_timeout(100); pg.screenshot(path=S + '19-plano.png', full_page=True)
        pg.click('#btnHome'); pg.click('.modo[data-ir=afina]'); pg.wait_for_timeout(100); pg.screenshot(path=S + '20-afina.png', full_page=True)
        pg.click('#btnHome'); pg.wait_for_timeout(200); pg.screenshot(path=S + '21-home-depois.png', full_page=True)
        b.close()

    if which in ('all', 'virtual'):
        b, pg = novo(p)
        pg.click('.modo[data-ir=hunt]'); pg.locator('#huntFases .chip').nth(2).click()
        pg.evaluate("tonica.Som.ligarVirtual()"); pg.wait_for_timeout(400)
        print('análise:', json.dumps(pg.evaluate("(() => { const i = tonica.Som.info(); return { caminho: i.caminho, taxa: i.taxa, janelaMs: +i.janelaMs.toFixed(1), passoMs: +i.passoMs.toFixed(1) }; })()")))
        pg.click('#huntGo'); pg.wait_for_timeout(2600)
        res = pg.evaluate("""async () => {
          const sleep = ms => new Promise(r => setTimeout(r, ms)), out = [];
          for (let i = 0; i < 16; i++) {
            let alvo = null; const tw = performance.now();
            while (!(alvo = document.querySelector('#huntBraco .ponto.alvo')) && performance.now() - tw < 4000) await sleep(16);
            if (!alvo) { out.push({ erro: 'sem alvo' }); break; }
            const midi = +alvo.getAttribute('data-midi'), antes = document.querySelector('#hPontos').textContent;
            await sleep(250 + Math.random() * 250);
            const t0 = performance.now(); tonica.Som.virtual(midi, 0.2);
            let dt = -1;
            while (performance.now() - t0 < 1500) { await sleep(3); if (document.querySelector('#hPontos').textContent !== antes) { dt = performance.now() - t0; break; } }
            out.push({ midi, dt: Math.round(dt) });
          }
          return out;
        }""")
        dts = sorted(r['dt'] for r in res if r.get('dt', -1) > 0)
        print('virtual guiado:', len(dts), 'de', len(res), 'acertos; ms até o placar mudar: mediana', dts[len(dts) // 2] if dts else None, 'máx', dts[-1] if dts else None)
        print('   ', res)
        # nota errada: semitom acima do alvo
        r2 = pg.evaluate("""async () => {
          const sleep = ms => new Promise(r => setTimeout(r, ms));
          let alvo; while (!(alvo = document.querySelector('#huntBraco .ponto.alvo'))) await sleep(16);
          const midi = +alvo.getAttribute('data-midi'), antes = document.querySelector('#hPontos').textContent;
          await sleep(700); tonica.Som.virtual(midi + 1, 0.2); await sleep(600);
          return { igual: document.querySelector('#hPontos').textContent === antes, fala: document.querySelector('#hFala').textContent };
        }""")
        print('virtual nota errada:', r2)
        pg.screenshot(path=S + '30-virtual.png')
        b.close()

    if which in ('all', 'arquivo'):
        b, pg = novo(p)
        pg.click('.modo[data-ir=afina]')
        pg.set_input_files('#aArq', W + 'soltas.wav'); pg.wait_for_timeout(8500)
        print('afinador/gravação:', txt(pg, '#aLatOut'), '| log:', txt(pg, '#aLog').replace('\n', ' '))
        print('cordas ok:', pg.locator('#aCordas li.ok').count(), '| diag:', txt(pg, '#aDiag').replace('\n', ' '))
        pg.screenshot(path=S + '31-afina-arquivo.png', full_page=True)
        b.close()

    if which in ('all', 'mic'):
        b, pg = novo(p, fake=W + 'soltas.wav')
        pg.click('#micBtn'); pg.wait_for_timeout(600)
        print('mic:', txt(pg, '#micTxt'), '| aviso oculto:', pg.locator('#aviso').is_hidden(), '| barra:', txt(pg, '#oModo'))
        pg.click('.modo[data-ir=afina]'); pg.wait_for_timeout(9000)
        print('mic falso -> cordas ok:', pg.locator('#aCordas li.ok').count(), '| log:', txt(pg, '#aLog').replace('\n', ' '))
        print('diag:', txt(pg, '#aDiag').replace('\n', ' '))
        pg.screenshot(path=S + '32-afina-mic.png', full_page=True)
        b.close()

    if which in ('all', 'desktop'):
        b, pg = novo(p, vp=(1280, 800))
        pg.screenshot(path=S + '40-desktop-home.png')
        pg.click('.modo[data-ir=hunt]'); pg.click('#huntGo'); pg.wait_for_timeout(2800); pg.screenshot(path=S + '41-desktop-hunt.png')
        b.close()
print('\nconsole:', logs[:12] if logs else 'sem erros')
