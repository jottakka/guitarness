# Pesquisa: fontes de cifras e tablaturas, leitura e notação

Data: 16/11/2026. Pesquisa assistida; este texto é pesquisa, não decisão. Verificado: termos de uso do Ultimate Guitar (UG), README do alphaTab e resumo do crate `guitarpro`. A busca dos termos de uso do Cifra Club retornou HTTP 403, então esses termos são desconhecidos.

## Fontes e questões legais

- Ultimate Guitar: não há API pública. Os termos (§6.2) proíbem duplicar ou reproduzir o conteúdo, exceto para impressão privada e não comercial. O §6.4 proíbe copiar e o §5.2(c) proíbe coletar informações.
- Songsterr: o estado da API é contraditório (no máximo metadados).
- Cifra Club: não há API conhecida. Os termos não foram lidos (403); é preciso lê-los à mão.
- Chordify: acordes derivados de áudio, sem API de cifras.
- Desenho de menor risco: entrada fornecida pelo usuário (colar o texto da cifra, soltar arquivos `.chordpro`, `.txt`, `.gp*`, MusicXML ou MIDI). Opcionalmente, "buscar URL": uma única requisição GET, iniciada pelo usuário, a uma página que ele indicou, com identificação honesta, respeitando o `robots.txt`, sem rastrear nem raspar buscas, e só local. Ainda assim isso pode contrariar o §6 do UG; deixar claro que a responsabilidade é do usuário. Guardar as cifras só na pasta de dados local, sem exportar nem compartilhar. Considerar descartar a letra (direitos autorais) e manter acordes, tempo e seções.
- Não há conjunto de dados aberto e verificado de cifras (o Chordonomicon foi raspado do UG, então é arriscado).

## Formatos e leitores

- ChordPro como formato interno de troca (chordpro.org; `{title}`, `{key}`, `{capo}`, `[Am]`). Escrever o leitor próprio (cerca de 200 linhas).
- Cifra em texto simples: classificar a linha de acordes (70 a 80% ou mais dos tokens casam com a expressão regular de acorde `[A-G][#b]?(m|maj|min|dim|aug|sus[24]?|add\d+|\d+)*(/[A-G][#b]?)?`, mais "°" e "(b5)"). Ancorar cada acorde ao deslocamento de coluna sobre a letra. Seções como "[Intro]" e "[Refrão]"; repetições como "(2x)"; blocos de tablatura do Cifra Club.
- Tablatura ASCII: detectar `^[eEBGDAa-g]\|[-0-9hpb/\\~x|()]+`, agrupar 6 linhas e usar as colunas como aproximação de tempo. Escrever o leitor próprio.
- Guitar Pro: o crate Rust `guitarpro` (slundi/scorelib; GP3/4/5 estável, GP6/7 experimental; licença não verificada); ruxguitar (player de GP em Rust, referência); alphaTab (MPL-2.0; JS/TS/.NET/Kotlin; lê GP3 a GP8 e MusicXML; exige atribuição visível).
- Existem crates Rust de MusicXML (não verificados). MIDI: `midly` (MIT/Apache, não verificado). O MIDI não traz informação de corda, então seria preciso um resolvedor de dedilhado.

## Renderização

| Opção | Licença | Adequação |
|---|---|---|
| alphaTab | MPL-2.0 | notação, tablatura e cursor; exige JS (webview) |
| VexFlow | MIT (não verificado) | notação e tablatura em SVG, JS |
| Verovio | LGPL-3.0 (não verificado) | SVG e mapa de tempo; tablatura limitada |
| OSMD | BSD-3 (não verificado) | MusicXML no navegador |
| LilyPond / MuseScore CLI | GPL | não embutir |
| Bevy próprio | n/a | cabeças de nota da fonte Bravura SMuFL (OFL 1.1) e números de tablatura em texto |

Recomendação: desenho próprio no Bevy (relógio, rolagem e detecção de acerto próprios). Na v1, notas e pausas simples. Plano alternativo: pré-renderizar SVG (Verovio ou VexFlow) para textura com resvg e um mapa de id para tempo.

## Cifra para fase jogável

- Seções viram compassos, em 4/4, com um acorde por compasso por padrão.
- Batida: biblioteca de padrões prontos por gênero e andamento. Inferir a batida a partir do áudio é nível de pesquisa.
- Andamento e pulsos: aubio (GPL), essentia (AGPL) e madmom (Python) trazem risco de licença. Na v1, usar toque de andamento e deslocamento; depois, Beat This! (MIT) via ONNX.
- Formas de acorde: tabela de formas (abertas primeiro, depois pestana); programação dinâmica (Viterbi) para minimizar o movimento; transposição por capotraste.
- Dedilhado: tablatura como escrita; para cifras só com acordes, gerar padrões de arpejo (P-I-M-A).
- Saída: JSON ou RON de fase, local.

## Pilha recomendada

JSON ou RON interno de fase, com importação e exportação ChordPro. Ordem de entrada: colar cifra, ChordPro, GP3/4/5, GP6/7 e MusicXML, e por fim a busca opcional de uma URL. Um agente de LLM estrutura em ChordPro o texto fornecido ou buscado. Leitores próprios de ChordPro, cifra e tablatura ASCII; `guitarpro` e `midly`. Grade de pulsos: toque de andamento na v1. Renderizador: Bevy próprio com Bravura.

Fontes: ultimate-guitar.com/about/tos.htm, github.com/CoderLine/alphaTab, docs.rs/crate/guitarpro/0.3.0, lib.rs/crates/guitarpro, agourlay.github.io/ruxguitar-tablature-player, songsterr.com/terms
