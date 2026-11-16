/* =====================================================================
   Topo, microfone, barra de entrada, laço principal e partida
   ===================================================================== */
function pintarMic() {
  const mic = Som.ligado() && Som.origem() === 'mic', b = $('#micBtn');
  b.classList.toggle('on', mic); b.setAttribute('aria-pressed', mic ? 'true' : 'false');
  $('#micTxt').textContent = mic ? 'Mic ligado' : 'Ligar mic';
  $('#oModo').textContent = mic ? 'Microfone' : Som.origem() === 'arquivo' ? 'Gravação' : Som.origem() === 'virtual' ? 'Violão virtual' : 'Modo toque';
  if (!Som.ligado()) { $('#oNivel').style.width = '0'; $('#oNota').textContent = 'toque nas bolinhas'; ouvidoTxt = ''; }
  if (Telas.afina && telaAtual === Telas.afina) Telas.afina.pintarDiag();
}
Som.aoMudar = pintarMic;

function avisar(txt) { $('#avisoTxt').textContent = txt; $('#aviso').hidden = false; }
$('#avisoX').addEventListener('click', () => { $('#aviso').hidden = true; });

$('#micBtn').addEventListener('click', async () => {
  if (Som.ligado() && Som.origem() === 'mic') { Som.desligar(); return; }
  try {
    await Som.ligarMic(Telas.afina.escolhido());
    $('#aviso').hidden = true;
    toast('Microfone ligado. Toque uma corda.');
    Telas.afina.listar();
  } catch (e) {
    const n = e && e.name;
    if (n === 'NotAllowedError' || n === 'SecurityError') avisar('O microfone foi bloqueado. Em um visualizador embutido isso é esperado: a página não recebe permissão de microfone. Jogue no modo toque (bolinhas do braço) ou abra o arquivo do jogo no navegador do computador, ou em um endereço https, para tocar com o violão.');
    else if (n === 'NotFoundError' || n === 'DevicesNotFoundError') avisar('Não encontrei microfone neste aparelho. Dá para jogar no modo toque.');
    else if (n === 'NotSupportedError') avisar('Aqui o navegador não oferece microfone: ele só funciona em https, em localhost ou com o arquivo aberto direto no computador. Dá para jogar no modo toque.');
    else avisar('Não consegui abrir o microfone (' + (n || 'erro desconhecido') + '). Dá para jogar no modo toque.');
  }
});

let ouvidoTxt = '';
function pintarOuvido() {
  if (!Som.ligado()) return;
  const fr = Som.ultimo; if (!fr) return;
  const db = 20 * Math.log10(Math.max(fr.rms, 1e-5));
  $('#oNivel').style.width = (clamp((db + 62) / 50, 0, 1) * 100).toFixed(0) + '%';
  $('#ouvido').classList.toggle('bate', agora() - Entrada.ultimoAtaque() < 110);
  const n = Entrada.tr.atual();
  let txt = 'ouvindo…';
  if (n != null) { const c = Math.round(Entrada.tr.cents()); txt = nomeNota(n % 12) + ' ' + (c >= 0 ? '+' : '−') + Math.abs(c) + ' cents'; }
  if (txt !== ouvidoTxt) { ouvidoTxt = txt; $('#oNota').textContent = txt; }
}

// navegação
$('#btnHome').addEventListener('click', () => ir('home'));
$$('.modo[data-ir]').forEach(b => b.addEventListener('click', () => ir(b.dataset.ir)));

// todo evento de nota (violão ou toque) vai para a tela da vez
Entrada.escutar(ev => { if (telaAtual && telaAtual.nota) telaAtual.nota(ev); });

// o áudio só pode nascer depois de um gesto do usuário
function destrava() { try { Som.garante(); } catch (e) { /* sem Web Audio */ } }
document.addEventListener('pointerup', destrava, { passive: true });
document.addEventListener('click', destrava, { passive: true });
document.addEventListener('keydown', destrava, { passive: true });

let antes = 0, avisouErro = false;
function laco(ts) {
  const dt = Math.min(0.05, (ts - antes) / 1000 || 0); antes = ts;
  try {
    Som.poll();
    if (telaAtual && telaAtual.update) telaAtual.update(ts, dt);
    pintarOuvido();
    FX.quadro(dt);
  } catch (e) { if (!avisouErro) { avisouErro = true; console.error(e); } }
  requestAnimationFrame(laco);
}

// alça de depuração no console: tonica.Som.info(), tonica.Som.ligarVirtual(), tonica.Som.virtual(57)...
window.tonica = { Som, Entrada, Base, Telas, Store, ir };

function iniciar() {
  pintarTopo(); pintarMic(); ir('home'); requestAnimationFrame(laco);
}
iniciar();
