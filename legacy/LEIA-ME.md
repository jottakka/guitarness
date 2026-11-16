# Demo web legada (Tônica)

Protótipo em JavaScript que deu origem ao núcleo em Rust. Hoje serve de referência: os testes do `tonica-dsp` comparam o detector em Rust com a saída guardada em `web-demo/test/golden/`. A demo é anterior ao nome Guitarness e ainda se chama Tônica. Veja o [README do repositório](../README.md).

## O que tem aqui

```
LEIA-ME.md              este arquivo
web-demo/
  src/                  fonte da demo (JavaScript, HTML e CSS)
  build.py              junta src/ em dist/tonica.html (gerado; não versionado)
  test/sim.js           simulação: violão sintético -> filtros -> worklet -> rastreador
  test/tom.js           bancada do estimador de tom
  test/golden.js        gera a referência para portar o detector
  test/golden/          a referência já gerada (WAV, JSON e CSV por cenário)
  test/gen_wav.py       gera os WAVs usados nos testes de navegador
  test/e2e.py, e2e2.py  testes no Chrome (toque, violão virtual, arquivo, microfone falso)
  test/wl.py            diagnóstico de carga do AudioWorklet por origem
```

## Abrir a demo

Gere o arquivo com `python3 build.py` (dentro de `web-demo/`) e abra `web-demo/dist/tonica.html` no Chrome, pelo arquivo ou por `http://localhost`. O microfone só funciona assim; dentro de um visualizador embutido o navegador pode recusar. Sem microfone, o modo toque funciona tocando nas bolinhas do braço.

## Comandos

A partir de `web-demo/`:

```
node test/sim.js                     simulação completa (SR=44100 ou SR=16000 muda a taxa)
node test/tom.js                     estimador de tom
node test/golden.js                  regenera test/golden
python3 build.py                     remonta dist/tonica.html

python3 test/gen_wav.py test/wav     WAVs dos testes de navegador (precisa de numpy)
python3 test/e2e.py                  precisa de: pip install playwright && playwright install chromium
python3 test/e2e2.py                 leva cerca de 2 minutos (espera uma rodada de 60 s acabar)
```

Os testes de navegador aceitam `CHROME=/caminho/do/chrome` e `URL=http://localhost:8000/tonica.html`. As capturas de tela vão para `web-demo/shots/`.

Conferido com Node 22 e Python 3.13.

## Linha de base

Saída de `node test/sim.js` a 48 kHz:

```
A isoladas                                  37/37   latência mediana 37 ms, p90 52 ms
A isoladas (mic de celular)                 37/37   36 ms, p90 52 ms
B soando, sem alvo                          22/23   55 ms, p90 60 ms
B soando, sem alvo (celular)                23/23   56 ms, p90 62 ms
C soando, com alvo, 1 nota a cada 0,5 s     23/23   30 ms, p90 40 ms
C soando, com alvo, 1 nota a cada 0,3 s     23/23   40 ms, p90 50 ms
D soando, com alvo, forte/fraco alternado   19/23   28 ms, p90 56 ms
E alvo = nota vizinha                       0 acertos falsos
F alvo uma oitava acima ou abaixo           0 acertos falsos
G ruído e batidas, sem nota                 0 eventos de nota
TOTAL                                       236/241 acertos, 0 falsos
```

- A 44,1 kHz: 231/241 e 1 falso. A 16 kHz: 241/241 e 0 falsos.
- Custo da análise: cerca de 270 µs por quadro em Node.
- Tom (`node test/tom.js`, perfis de Temperley): conjunto de notas certo em 95 de 120 músicas sintéticas com baixo e melodia e em 100 de 120 só com acordes.
- No Chrome sem tela (contexto de áudio a 44,1 kHz), com o violão virtual, do som injetado até o placar mudar: mediana de 30 a 39 ms; 16 de 16 notas no modo guiado.

Tudo isso é com sinal sintético. Nada foi testado com violão real nem em celular.

## A referência para o porte (`test/golden/`)

Cinco cenários. Para cada um:

- `<caso>.wav`: mono, 16 bits, 48 kHz. É o sinal "de microfone", antes dos filtros.
- `<caso>.json`: `parametros`, `tocadas` (instante, nota MIDI, amplitude e, quando há, o instante em que a corda é abafada), `alvos` (instante em que o jogo avisa o detector de cada alvo), `quadros` (quantos) e `eventos`. `wav_sha256` identifica o WAV.
- `<caso>.quadros.csv`: uma linha por quadro do detector: `t, f, c, rms, rmsS, tid, p`. `p` traz a presença de cada alvo, separada por ponto e vírgula; fica vazia quando não há alvo.
- `RESUMO.txt`: o placar de cada cenário.

```
A-isoladas       37/37   0 eventos via alvo, 37 via mpm   1648 quadros
B-soando         23/23   0 via alvo, 23 via mpm           1167 quadros
C-alvo-rapido    22/23   22 via alvo, 3 via mpm            737 quadros
D-forte-fraco    20/23   20 via alvo, 0 via mpm           1167 quadros
F-oitava-errada   9/9    0 via alvo, 9 via mpm             513 quadros
```

O cenário F existe para provar o contrário: o alvo é a oitava de cima da nota tocada e não pode sair nenhum evento via "alvo".

### O pipeline exato, para reproduzir

1. Leia o WAV como f32 (`amostra / 32768`).
2. Filtros biquad com as fórmulas RBJ, as mesmas do `BiquadFilterNode`: passa-altas em 62 Hz com Q 0,707, depois dois passa-baixas em 2600 Hz com Q 0,707.
3. Processe em blocos de 128 amostras. O tempo do bloco é `bloco * 128 / 48000`.
4. Antes de cada bloco, aplique os alvos cujo instante já passou: o identificador `tid` sobe de 1 em 1, o detector recebe as frequências e o rastreador recebe `setTargets(midis, tid, tempo_do_bloco)`.
5. Decimação por 2, com média simples de duas amostras, para 24 kHz. A janela é um anel de 1024 amostras.
6. A cada 257 amostras decimadas, um quadro: `analyze(janela, 257)` e `presence(f)` para cada alvo. O tempo do quadro é `tempo_do_bloco + i / 48000`, em que `i` é o índice, dentro do bloco, da amostra que completou o passo.
7. Cada quadro vai para `makeTracker(...).push(quadro)`, que emite os eventos.

Detalhes que mudam o resultado:

- Os buffers são f32, mas as somas (energia, autocorrelação) são feitas em precisão dupla e só o resultado da NSDF volta para f32.
- Nos filtros, coeficientes e estados são em precisão dupla; a saída de cada filtro é arredondada para f32 antes do filtro seguinte.
- Os limiares do rastreador contam quadros de 10,7 ms. Se o passo mudar, converta para tempo.
- `node test/golden.js` é determinístico (gerador com semente fixa). Se o `wav_sha256` de um WAV regenerado for diferente do que está no JSON deste diretório, o seu Node gerou outro sinal: use o par WAV e JSON da mesma execução.

## Limites conhecidos da demo

- Nota fraca tocada sobre nota forte que ainda soa é perdida em parte dos casos (cenário D).
- Acorde rasgueado não é avaliado: o Monta-Acorde ouve as notas uma a uma.
- Não há julgamento de ritmo nem importação de tablatura.
- Sem fone de ouvido, a base toca só bateria para não confundir o microfone.
- O tom estimado erra cerca de 1 em cada 5 músicas sintéticas; por isso a tela deixa trocar o tom.
