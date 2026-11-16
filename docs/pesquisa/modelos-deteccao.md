# Pesquisa: modelos já treinados para detecção no Guitarness (16/11/2026)

Pesquisa assistida. Todas as páginas foram lidas em 16/11/2026 por ferramenta de leitura web que devolve resumo automático. Números e licenças abaixo são do que a página mostrou; o que não apareceu está marcado **(NV = não verificado)**. Nada foi baixado, instalado ou executado. Não repete [detecção de acordes e cordas](deteccao-acordes-cordas.md) nem [ecossistema Rust de áudio](ecossistema-rust-audio.md).

## 0. Conclusão em cinco linhas

1. **Nenhum modelo pronto serve, sozinho, para reprovar uma corda.** Os melhores resultados públicos de tablatura em violão acústico (GuitarSet) ficam em F1 de tablatura ~0,75 a 0,79; o TART (ISMIR 2026) cai para 71,8% de F1 string-fret e 54,08% ponta a ponta em teste zero-shot. Isso é incompatível com "falsa acusação é o pior erro".
2. **Os pesos de tablatura com licença clara praticamente não existem.** FretNet e TabCNN publicam código (FretNet: MIT) sem pesos; Trimplexx não publica pesos; GAPS não decidiu liberar pesos; TART não mostra repositório. O único repositório com pesos de tablatura é o de Reyhanlioglu (código MIT, **licença dos pesos não declarada**).
3. **O que existe e é utilizável hoje:** Basic Pitch (Apache-2.0, 230 KB em ONNX), SwiftF0 (MIT no código, 96 mil parâmetros, ONNX), Beat This! (MIT, checkpoints de 8 MB e 78 MB, só PyTorch), ChordMini (MIT no código, ONNX de terceiro). Servem como **segunda opinião de presença de notas** e como medida na bancada, não como juiz.
4. **Os dados de treino abertos com rótulo por corda são poucos e licenciáveis:** GuitarSet (CC BY 4.0), EGDB (CC BY 4.0), Guitar-TECHS (CC BY 4.0), EGFxSet (CC BY 4.0), AG-PT-set (CC BY 4.0). Isso sustenta a rota "treinar a nossa rede pequena e causal" do ADR-004 sem risco de licença. Armadilhas: IDMT-SMT-Guitar é CC BY-NC-ND; GAPS é CC BY-NC-SA com termo de assinatura (só pesquisa).
5. **Proposta para o teste do juiz:** medir Basic Pitch e SwiftF0 sobre as gravações do mantenedor como linha de base ML barata; treinar a rede `StringNet` do DESIGN §1.3 (C3) sobre os conjuntos CC BY; só rodar os pesos de Reyhanlioglu como verificação lenta, depois de pedir a licença ao autor.

## 1. Lista curta: os 5 para avaliar primeiro

Critério: licença limpa, CPU, ajuste ao papel "verificar o acorde/cordas esperados" (e não "transcrever"), esforço de integração em Rust.

| # | Modelo | Papel no Guitarness | Licença (código / pesos) | CPU | Por quê |
|---|---|---|---|---|---|
| 1 | **Basic Pitch** (Spotify) | Segunda opinião polifônica: "esta classe de altura está presente após o ataque?" Faixa lenta, fora do callback. | Apache-2.0 / Apache-2.0 (repositório declara; ONNX re-hospedado igual) | 230 KB (ONNX), 17 a 36 mil parâmetros (fontes divergem). Latência: **nenhuma medida publicada**; medir. | O menor e mais permissivo. ONNX oficial. Entrega ativação de notas e onsets por frame. Não dá corda, só altura. F1 de onset 79,0% na GuitarSet (tabela do artigo GAPS) é o mais fraco entre os comparados, então só entra como evidência extra, nunca como único voto. |
| 2 | **Pesos da cascata HCQT+Mel** (Reyhanlioglu, repositório `Guitar-Transcription` + `Guitar-Transcription-Weights`) | Verificação lenta de corda/casa nas gravações do mantenedor, nunca em tempo real. | Código MIT / **pesos: licença não declarada (pedir ao autor)** | 6,65 M parâmetros; fator de tempo real **0,615 em CPU com 6 threads, float32 (medido pelo autor, sem extração de features)**; precisa de ~209 ms de contexto futuro. | Único conjunto de pesos de tablatura achado: F1 de tablatura 0,776 ± 0,034 e taxa de desambiguação 0,943 em validação cruzada leave-one-player-out na GuitarSet (seis dobras, pesos das seis). Serve para medir quanto uma rede de tablatura erra nas gravações reais. Export ONNX não mencionado (NV). |
| 3 | **SwiftF0** | Altura de nota única (exercícios de uma corda) e raiz grave; confiança de voz. | Código MIT (página do GitHub); **licença dos pesos não declarada** | 14.386 parâmetros (README) ou 95.842 (artigo); README: ~180x tempo real em 1 núcleo de laptop; 16 ms por quadro, 176 ms de atraso para finalizar quadros (**medido pelo autor, CPU não nomeada**). ONNX nativo. | Melhor custo-benefício para nota única e já em ONNX. Treinou em NSynth, MIR-1k, PTDB-TUG, MDB-STEM-Synth: **sem violão real**. Monofônico: não serve para acorde. |
| 4 | **ChordMini ChordNet 2E1D / BTC** (ptnghia-j) | Identidade do acorde como voto do caminho B do juiz (§1.3). | Código MIT; **pesos: licença não declarada** | Tamanho/latência NV. Existe ONNX de terceiro (`musetric/chordmini-onnx`, vocabulário de 170 classes, feito para navegador). | Já em ONNX; vocabulário maior que o da literatura de rasgueado (24 classes). Treinado por pseudo-rótulos em FMA, DALI e MAESTRO (música com direitos autorais; **risco de proveniência**). Precisão publicada: nenhuma na página. Só avaliar se a bancada mostrar ganho sobre croma+baixo. |
| 5 | **Rede própria tipo TabCNN/FretNet, causal, treinada por nós** | O C3 `StringNet` do DESIGN. | MIT (FretNet), TabCNN (NV: GitHub deu 404) / nossos pesos | TabCNN é pequena (CNN sobre CQT). Sem medida de CPU publicada. | Não é "modelo pronto", mas é a única rota com dados de licença limpa. Linha de base: TabCNN tablatura F1 0,748 ± 0,047, MPE 0,826 na GuitarSet; EGSet12 fora de domínio: TDR 0,695, sobe para 0,776 com Guitar-TECHS. Isso mostra que a diversidade de hardware importa mais que a arquitetura. |

Ficaram fora do top 5, de propósito: PESTO (LGPL-3.0, ver §4), YourMT3+ (GPL-3.0, ver §4), MuScriptor (pesos CC BY-NC, 1,3 B parâmetros), MT3 (grande, sem pesos com licença declarada na página), CREPE (pesado, treinado sem violão), TART e Noise2Fret (sem código achado), Murgul (sem pesos).

## 2. Tabela completa

Legenda de exportação para Rust: ONNX/ort = `ort` 2.0.0-rc.15; tract = `tract-onnx` 0.23.10 (ambos de [ecossistema Rust de áudio](ecossistema-rust-audio.md)). "NV" = não verificado. Nenhum modelo foi testado no tract; operadores exóticos (GRU bidirecional, LSTM, atenção) podem falhar nele **(NV)**.

### 2.1 Transcrição de violão para tablatura (corda/casa)

| Modelo | Saída | Precisão relatada (dataset) | Licença código / pesos | Tamanho / CPU | Exportação Rust | Dados de treino (licença) | Ajuste: verificar vs transcrever |
|---|---|---|---|---|---|---|---|
| TabCNN (Wiggins & Kim, ISMIR 2019) | Por corda, softmax sobre casas, por frame | Tablatura F1 0,748; MPE F1 0,826; TDR 0,899 (GuitarSet, validação cruzada; números de reexecução no artigo Guitar-TECHS) | NV (repositório `andywiggins/tabcnn` deu 404 na minha tentativa) / sem pesos achados | Pequena; sem medida de CPU | PyTorch/Keras; ONNX a fazer | GuitarSet (CC BY 4.0) | Boa arquitetura de partida para verificar: saída por corda casa com "esperado". Não reprova sozinha (F1 ~0,75). |
| FretNet (Cwitkowitz et al., ICASSP 2023, arXiv 2212.03023) | Contorno contínuo de altura agrupado por corda e casa | Artigo diz "competitivo" com a linha de base; sem números na página | MIT (código) / **sem pesos** ("TODO" no README) | NV | PyTorch; ONNX a fazer | GuitarSet | Idem; o contorno dá desvio em cents por corda, útil para "casa errada". |
| Reyhanlioglu (cascata HCQT+Mel, multitarefa) | Grade 6 cordas x 21 estados de casa + decodificadores auxiliares | Tablatura F1 0,776 ± 0,034; TDR 0,943 (GuitarSet, 6 dobras leave-one-player-out). TPMNet citado com 0,788. | MIT / **não declarada** (pesos em repositório separado, seis dobras) | 6,65 M parâmetros; RTF 0,615 CPU 6 threads f32 (**medido pelo autor**); ~209 ms de futuro | ONNX NV; PyTorch | GuitarSet | Verificação lenta. Latência de ~209 ms + features é incompatível com veredito dentro da janela do golpe, mas serve para o veredito tardio. |
| Trimplexx (CRNN, CQT 168 bins, 2 BiGRU 768) | Onset + casa por corda; ASCII/MIDI | MPE F1 0,8736; TDR F1 (tablatura nota) 0,8569; onset ~0,85 (GuitarSet; **a página não diz se é no split de teste de 36 faixas**) | MIT / **pesos não publicados** | Não informado; BiGRU 768 é grande; CUDA no exemplo | Só PyTorch | GuitarSet | Receita de referência. Os números altos (0,87) vêm de um repositório com 20 estrelas, auto-relatado, sem avaliação independente. Não confiar. |
| GAPS (Riley et al., ISMIR 2024) | Notas MIDI (sem corda) | Onset F1 (50 ms) na GuitarSet: 91,2% supervisionado (GAPS+GuitarSet); 88,1% zero-shot (só GAPS). Basic Pitch 79,0%; MT3 90,0% (supervisionado) e 32,0% (zero-shot). | Dataset CC BY-NC-SA 4.0 com termo assinado, só pesquisa / **pesos: "considerando se libera"**; sem código | NV (CRNN estilo Kong et al.) | n/a | GAPS (violão clássico, 14 h) | Não é utilizável (sem pesos) e dá nota, não corda. |
| TART (Gupta et al.; arXiv 2510.02597 e 2609.11904, ISMIR 2026) | Notas + técnica (9 classes) + corda/casa (T5) + tab | Zero-shot média em GuitarSet, EGDB e versões ruidosas: MIDI F50 81,35%, tab string-fret F1 71,8%, ponta a ponta 54,08% | Artigo CC BY 4.0 / **código e pesos: nada achado** | Pipeline de 4 estágios, T5: pesado | n/a | NV | Mostra o estado da arte real: 54% ponta a ponta. Confirma a revisão independente anterior. |
| Noise2Fret (Simionato & Bigo, arXiv 2608.30854, ISMIR 2026) | Tablatura por difusão | "Supera linhas de base"; sem números na página (GuitarSet, GOAT) | Artigo CC BY-SA 4.0 / nada achado | "Mais eficiente" (qualitativo) | n/a | GuitarSet, GOAT | Sem pesos; difusão não é causal. Descartar para tempo real. |
| Fretting-Transformer (arXiv 2506.14223) | MIDI para tab (simbólico) | n/d | n/d | n/d | n/a | n/a | Entrada é MIDI, não áudio. Só útil para sugerir pegadas. |

### 2.2 Transcrição geral (AMT)

| Modelo | Saída | Precisão (dataset) | Licença código / pesos | Tamanho / CPU | Exportação | Treino (licença) | Ajuste |
|---|---|---|---|---|---|---|---|
| **Basic Pitch** (ICASSP 2022) | Notas, onsets, contorno de altura (polifônico, instrumento-agnóstico) | Onset F1 79,0% na GuitarSet (50 ms; **tabela do artigo GAPS**, split supervisionado) | **Apache-2.0** / Apache-2.0 | 230 KB ONNX (terceiros), 17 a 36 mil parâmetros (fontes divergem). Latência não publicada. Entrada 22.050 Hz. | ONNX oficial no repositório; também TFLite, CoreML. `ort` ou `tract` provável (NV) | Dados diversos; lista exata NV nesta leitura | Segunda opinião de classes de altura. Falso aceite alto em acorde denso. |
| MT3 (Google Magenta, ICLR 2022) | Notas MIDI multi-instrumento | 90,0% onset F1 (GuitarSet supervisionado) / 32,0% (zero-shot), do artigo GAPS | Apache-2.0 (código) / **pesos: NV** | T5, grande; treino "não é fácil" | JAX/T5X; sem ONNX | Slakh, MAESTRO e outros | Pesado demais; fora. |
| YourMT3+ (MLSP 2024) | Notas multi-instrumento | NV | **GPL-3.0 (código)** / pesos NV | NV | PyTorch | NV | **Bloqueado pela regra GPL** (código). Os pesos precisariam de licença própria; não avaliar. |
| MuScriptor (2026, Mirelo + kyutai) | Notas MIDI multi-instrumento | F1 onset 60,4, frame 72,4 (agregado, D_Test, 372 faixas; **sem número de violão**) | Código MIT / **pesos CC BY-NC 4.0, acesso fechado por aceite** | 1,3 B parâmetros | NV | Pop e clássico ocidental | **Bandeira vermelha:** pesos não comerciais, enorme. Fora. |

### 2.3 Altura monofônica e polifônica

| Modelo | Saída | Precisão (dataset) | Licença código / pesos | Tamanho / CPU | Exportação | Treino | Ajuste |
|---|---|---|---|---|---|---|---|
| **SwiftF0** (Nieradzik, arXiv 2508.18440) | f0 por quadro de 16 ms, 46,9 a 2093,75 Hz, confiança | F1 93,20 limpo / 89,53 com ruído a 10 dB (Vocadito, Bach10-mf0-synth, SpeechSynth); CREPE 79,30/75,13 | **MIT** (README) / pesos NV | 14.386 parâmetros (README) vs 95.842 (artigo); 132,6 ms para 5 s de áudio (**medido pelo autor, "desktop moderno"**); ~180x tempo real (README) | ONNX nativo | NSynth, PTDB-TUG, MIR-1k, MDB-STEM-Synth, SpeechSynth, ruído CHiME-Home (licenças não informadas) | Nota única. Sem violão no treino. Não polifônico. |
| **PESTO** (Sony CSL, ISMIR 2023 / TISMIR 2025) | f0 por quadro, streaming | Nível de CREPE com 800x menos parâmetros (texto do repositório; números NV) | **LGPL-3.0** (código, LICENSE.md) / pesos NV | 30 mil (2023) ou 130 mil (2025) parâmetros; ONNX ~0,7 ms/inferência (**do README, hardware não informado**); i7-1185G7, ~12x tempo real | ONNX (`realtime.export_onnx`), com cache de streaming por conta do chamador | MIR-1K e outros | **Bandeira**: LGPL-3.0 (ver §4). Só pesos ONNX poderiam ser usados, mas a licença dos pesos é NV. |
| CREPE (2018) | f0 por quadro | NV | MIT / MIT (repositório) | Cinco tamanhos (tiny a full); ~22 M parâmetros (full) | Keras/TensorFlow; ONNX não mencionado | MIR-1K, Bach10, RWC-Synth, MedleyDB, MDB-STEM-Synth, NSynth | Mais pesado e menos robusto que SwiftF0 na comparação do artigo SwiftF0. |
| FCPE (arXiv 2509.15140) | f0 | RPA 96,79% na MIR-1K (**viés: RMVPE, CREPE e PESTO treinaram na MIR-1K**) | MIT (relato de terceiro) | 10,64 M parâmetros; RTF 0,0062 em RTX 4090 (**GPU, do autor**) | PyTorch | NV | Sem números de CPU; fora. |
| RMVPE | f0 robusto | NV | "MIT com cláusula de pesquisa" (relato de terceiro, NV) | 90,42 M parâmetros (relato) | n/d | n/d | Pesado e licença ambígua; fora. |

### 2.4 Onset, tempo e batida

| Modelo | Saída | Precisão | Licença código / pesos | Tamanho / CPU | Exportação | Treino | Ajuste |
|---|---|---|---|---|---|---|---|
| **Beat This!** (ISMIR 2024) | Batidas e tempos fortes sem pós-processamento DBN | Números nas Tabelas 1 a 3 do artigo; não obtidos | **MIT** (código **e** pesos, declarado) / ver nota de dados | `small*` ~8,1 MB; `final*` ~78 MB; CPU NV | PyTorch Lightning; ONNX só por terceiros (`musetric`, `ashudesai`) | 16 conjuntos; README avisa que parte dos arquivos tem direitos autorais ou CC limitado | Sincronizar faixa de acompanhamento ou metrônomo, não julgar o jogador. Processa arquivo inteiro (não causal). |
| madmom | Onset/batida/acorde | NV | **Código BSD, modelos CC BY-NC-SA (de memória, NV nesta sessão)** | n/d | Python | n/d | **Bandeira (NV):** evitar. A página do Beat This! em terceiros também cita o DBN do madmom como uso não comercial. |
| Onset por fluxo espectral (DESIGN §1.3 C0) | Onset | n/d | Código nosso | CPU trivial | Rust nativo | n/a | Já é o plano. Nenhum modelo de onset treinado achado que melhore isso com licença limpa. |

### 2.5 Reconhecimento de acorde

| Modelo | Saída | Precisão | Licença | Tamanho / CPU | Exportação | Treino | Ajuste |
|---|---|---|---|---|---|---|---|
| ChordMini (BTC, ChordNet 2E1D) | Acorde por trecho (170 classes no ONNX de terceiro) | Nenhuma na página | Código MIT / pesos NV | NV | ONNX por `musetric/chordmini-onnx` (terceiro) | Pseudo-rótulos em FMA, DALI, MAESTRO; rótulos reais sob acesso restrito | Voto de identidade para acorde fora do conjunto de confusão. Treinado em mistura de música, não em violão solo; precisão em violão sem eco NV. |
| BTC no teste de Murgul | Acorde maior/menor | 89,21% no microfone (vs 90,06% do CRNN deles e 80,37% Deep Chroma) | NV | NV | NV | NV | Mostra teto ~90% de acorde só com áudio de microfone. |
| accordoai (PyPI) | Classificador de acorde | NV | MIT (listagem) | NV | sem ONNX | NV | Fraco; os autores dizem que ruído e acordes raros falham. |
| Chordino / NNLS-Chroma | Acorde | n/d | **GPL** (de memória, NV nesta sessão) | n/d | n/d | n/d | **Bloqueado** se confirmado. |

### 2.6 Técnica de execução (abafado, palm mute, slide, hammer-on, bend, direção)

| Modelo ou recurso | Saída | Precisão | Licença | Observação |
|---|---|---|---|---|
| Murgul, Schimper, Heizmann (ISMIR 2025, arXiv 2508.07973) | Início do rasgueado, direção, acorde (24 classes) | Rasgueado F1 92,75% (mic); direção: baixo 85,51%, cima 79,02% (mic); acorde 90,06% | Artigo CC BY 4.0 / **dados, código e pesos: nada achado** | CRNN com BiGRU, não causal, sem latência publicada. Só serve como teto de referência: cima ~79% no áudio é insuficiente para reprovar. |
| TART, estágio de técnica | 9 classes + "outra" (slide, bend, percussivo...) | NV | Sem código achado | Autores relatam sobreajuste entre bases. |
| SpectroFusionNet (Sci. Reports 2025) | 9 classes elétricas (inclui bend, hammer-on, slide) | Alta nas classes citadas; **sem palm mute**; 549 clipes | NV | Base minúscula; não generaliza. |
| Guitar-TECHS / AG-PT-set / EG-IPT (dados, ver §3) | Rótulos | n/d | CC BY 4.0 (Guitar-TECHS, AG-PT-set) | Só dados; nenhum modelo pronto de palm mute ou abafado achado. |

**Conclusão da seção:** não existe modelo pronto e licenciado para abafado, palm mute, slide, hammer-on ou direção. O "abafado" do juiz (`Muffled`, decaimento curto) continua sendo DSP nosso; a direção continua dica fora do Palco, como no DESIGN §1.4.

### 2.7 Avaliação de desempenho musical (performance assessment)

Nada serve ao Guitarness hoje. Achados, todos de **piano** ou de IA que dá nota estética:
- Dhiman 2026 (arXiv 2601.19029): MuQ/MERT para 19 dimensões de qualidade de piano; R² 0,537; pipeline e modelos "liberados" (licença NV).
- MAJEPPA (arXiv 2608.11026): MIDI simbólico, piano.
- LLaQo (ICASSP 2025, Vicuna-7B): treinado só em piano.
- ICASSP 2026 Song Aesthetics: nota de música gerada por IA, não de aluno.
Nenhum é para violão, nenhum julga acerto de corda, e uma nota perceptual contínua é o oposto do veredito discreto e conservador que o jogo precisa. **Descartar.**

## 3. Conjuntos de avaliação e treino

| Conjunto | Conteúdo | Rótulo por corda | Licença | Lido em | Observação |
|---|---|---|---|---|---|
| **GuitarSet** (Xi et al., 2019) | Violão acústico, 6 tocadores, captadores hexafônicos; áudio de microfone, mix do captador, captador original e "debleeded" | Sim (hexafônico, JAMS) | **CC BY 4.0** (Zenodo 3371780; 8,2 GB; atualizado 24/04/2025) | 16/11/2026 | Principal. Sem direção de rasgueado. Zenodo e GitHub podem diferir (o MIT citado em [detecção de acordes e cordas](deteccao-acordes-cordas.md) vale só para o código do GitHub). |
| **EGDB** (Chen et al., ICASSP 2022) | Elétrica, 240 tablaturas, seis timbres re-amplificados, DI hexafônico | Sim | **Página do Zenodo 7044411 que li é de outro conjunto (EGFxSet): CC BY 4.0, 5,8 GB. A licença do EGDB em si: NV.** Artigo posterior diz "CC", sem variante. | 16/11/2026 | Confirmar o registro certo do EGDB antes de treinar. |
| **EGFxSet** (Pedroza et al., ISMIR 2022) | Strat limpa com 12 efeitos de hardware, 22 casas, 5 captadores | Nota única por casa | **CC BY 4.0** (Zenodo 7044411) | 16/11/2026 | Bom para robustez a timbre; nota única, não acorde. |
| **Guitar-TECHS** (ICASSP 2025) | Elétrica, 5 h 12 min, 4 configurações de captura, MIDI por corda (Triple Play); inclui palm mute, bend, vibrato, harmônicos | Sim (MIDI multitrilha por corda) | **CC BY 4.0** (site e artigo; registro Zenodo 14963133, campo de licença NV) | 16/11/2026 | Melhorou TDR fora de domínio em ~8 pontos no TabCNN. Sem vídeo. |
| **IDMT-SMT-Guitar** (Zenodo 7544110) | 7 violões, ~4.700 notas, técnicas, acordes, 64 peças | Notas e técnicas, sem hexafônico | **CC BY-NC-ND 4.0** | 16/11/2026 | **Bandeira:** não comercial e sem derivados. Só avaliação interna; não treinar nem redistribuir. |
| **GAPS** (QMUL, ISMIR 2024) | Violão clássico, ~14 h, 200+ tocadores, MIDI alinhado | **Não** (só nota) | **CC BY-NC-SA 4.0 (Zenodo 17152440, sem áudio) / termo assinado "somente o indivíduo", pesquisa não comercial** (página GAPS); versão HF com áudio recomendada | 16/11/2026 | **Bandeira:** não comercial. Não usar para treino de produto sem permissão escrita. |
| **AG-PT-set** (Zenodo 10159492) | Violão de aço, 15 h 55 min, 12 técnicas (inclui palm mute, bending, hammer-on), 6 tocadores | Não (nota única mono) | **CC BY 4.0**; 6,7 GB | 16/11/2026 | Só ~10 h rotuladas (8 técnicas). Útil para "abafado/palm mute" no futuro. |
| **EG-IPT** (NIME 2025) | Elétrica, 19 técnicas (slide agrupado com legato) | NV | NV | 16/11/2026 | Licença NV. |
| **SynthTab** (Zang et al., ICASSP 2024) | Áudio sintetizado do DadaGP | Sim (da tablatura) | NV (página só mostra licença do artigo) | 16/11/2026 | DadaGP deriva de tablaturas de terceiros: **risco de proveniência**, NV. |
| Dados de Murgul (rasgueado) | 90 min reais (IMU) + ~4 h sintéticos | Direção e acorde | **Nada achado** | 16/11/2026 | Não publicados que eu tenha visto. |
| GOAT (usado pelo Noise2Fret) | NV | NV | NV | 16/11/2026 | Não investigado. |

## 4. Bandeiras vermelhas de licença

| Item | Problema | Ação |
|---|---|---|
| **YourMT3+** | Código **GPL-3.0** | Proibido sem aprovação do mantenedor. Fora. |
| **PESTO** | Código **LGPL-3.0**; licença dos pesos NV | LGPL não está na lista de proibidos, mas é cópia-esquerda fraca: pedir aprovação ao mantenedor antes de qualquer uso, mesmo só com pesos ONNX. SwiftF0 cobre o mesmo papel com MIT. |
| **MuScriptor** | Pesos **CC BY-NC 4.0**, acesso gated | Não comercial. Fora. |
| **IDMT-SMT-Guitar** | **CC BY-NC-ND 4.0** | Só avaliação interna, sem redistribuir nem treinar produto. |
| **GAPS** | **CC BY-NC-SA 4.0**, termo individual | Não usar em produto. |
| **madmom** (modelos) | CC BY-NC-SA (**de memória, NV**) | Evitar até checar. |
| **Chordino/NNLS-Chroma** | GPL (**de memória, NV**) | Evitar até checar. |
| **Pesos sem licença declarada** | SwiftF0, ChordMini, Reyhanlioglu, Trimplexx: o repositório declara MIT para o **código**; os pesos não dizem nada | "Sem licença" legalmente = todos os direitos reservados. Pedir ao autor (issue no GitHub) e registrar a resposta antes de embarcar no produto. Para avaliação interna no teste do juiz não é bloqueio. |
| **Pesos treinados em música com direitos autorais** | ChordMini (FMA, DALI, MAESTRO), Beat This! (README alerta arquivos protegidos) | Risco de proveniência; fora do alcance deste relatório. |
| **Basic Pitch** | O README cita "GPL License" no rodapé sem explicar | Provavelmente aviso de dependência, mas confirmar o LICENSE e o arquivo NOTICE. Apache-2.0 declarado. |
| **Rotas `ort` e `tract`** | `ort` ainda é `rc`; `tract` pode não suportar todos os operadores | Ver [ecossistema Rust de áudio](ecossistema-rust-audio.md). Medir no spike. |

## 5. Limites desta pesquisa (o que não consegui verificar)

- Latência em CPU de Basic Pitch, TabCNN, FretNet, ChordMini, Beat This!, CREPE, YourMT3: **não há medida publicada** que eu tenha achado. Os únicos números de CPU são do próprio autor: SwiftF0 (~180x tempo real; 132,6 ms/5 s), PESTO (~0,7 ms ONNX; ~12x tempo real num i7-1185G7), Reyhanlioglu (RTF 0,615, 6 threads).
- Licença dos **pesos** de SwiftF0, PESTO, ChordMini, Reyhanlioglu, Trimplexx, TabCNN, FretNet, MT3: não declarada nas páginas lidas.
- Repositório do TabCNN (`andywiggins/tabcnn`) devolveu 404; licença NV.
- GAPS: sem código nem pesos; licença do dataset difere entre Zenodo e a página do projeto; página HF (`karaokenet/GAPS` que tentei) deu 401, não é a URL certa.
- EGDB: o registro do Zenodo que li é do EGFxSet; licença do EGDB NV.
- Tabelas e números do artigo TART (além do resumo), do Noise2Fret e do FretNet: só o resumo foi lido.
- Beat This! (Tabelas 1 a 3), SwiftF0 em violão, Basic Pitch em violão: sem números próprios; o 79,0% de Basic Pitch vem do artigo GAPS, não do dos autores.
- madmom e Chordino: licenças vêm da minha memória, não de página lida nesta sessão.
- Os resumos das páginas vieram de um modelo auxiliar do WebFetch. Dados sensíveis (números, licenças) devem ser reconferidos na fonte antes de entrarem em ADR.
- Datas "última atualização" dos repositórios: a ferramenta não mostrou. Conferir se o projeto está vivo.

## 6. Próximos passos concretos (teste do juiz com as gravações do mantenedor)

1. **Bancada de linha de base (sem treino).** Sobre as gravações do mantenedor (golpes certos, corda a menos, corda a mais, abafados): rodar Basic Pitch via `ort` (arquivo oficial `nmp.onnx`, 230 KB) e medir, por golpe, a ativação das classes de altura esperadas e estranhas. Registrar falso aceite e falso reprovado. Medir tempo por bloco (meta < 10 ms por bloco de áudio; se estourar, fica na faixa lenta).
2. **SwiftF0 em ONNX** para as notas únicas e a raiz: comparar com o MPM do `presence` (M1) em precisão e em latência. Pedir ao autor a licença dos pesos (issue no GitHub).
3. **Cascata do Reyhanlioglu como verificadora lenta.** Pedir licença dos pesos ao autor; só então baixar as dobras. Rodar nas gravações do mantenedor em CPU (6 threads) e comparar a decisão por corda com a LLR do DSP (DESIGN §1.3 C2). Critério: de quantos falsos reprovados o DSP seria salvo. Se a rede errar tanto quanto o DSP, não entra.
4. **Treino próprio do `StringNet` (C3).** Combinar GuitarSet + Guitar-TECHS + EGFxSet (e EGDB depois de confirmar licença) + golpes sintéticos das nossas pegadas, todos CC BY 4.0. Checar a regra de entrada do ADR-004: cortar ≥ 30% do erro aceito mantendo reprovação falsa ≤ 2% em dados reais separados.
5. **Gravar o conjunto de validação interno** (já previsto) com rótulo por corda (captador hexafônico, se o mantenedor tiver; senão rótulo manual). Nenhum conjunto público tem o violão, o microfone e o ambiente do mantenedor.
6. **Decisão de licença para o mantenedor:** (a) aprovar ou recusar PESTO (LGPL-3.0); (b) autorizar contato com autores de SwiftF0, ChordMini e Reyhanlioglu para licenciar os pesos; (c) confirmar que YourMT3+ e MuScriptor ficam fora.
7. **Direção de rasgueado:** nenhum modelo pronto; manter áudio como dica e câmera como desempate (DESIGN §1.4), e reavaliar se Murgul liberar pesos.

## 7. Fontes (todas lidas em 16/11/2026)

- GuitarSet: https://zenodo.org/records/3371780
- IDMT-SMT-Guitar: https://zenodo.org/records/7544110
- GAPS: https://arxiv.org/abs/2408.08653 ; https://arxiv.org/html/2408.08653v2 ; https://aim-qmul.github.io/GAPS ; https://zenodo.org/records/17152440
- Guitar-TECHS: https://arxiv.org/html/2501.03720 ; https://zenodo.org/records/14963133 (via busca)
- EGFxSet: https://zenodo.org/records/7044411 ; EGDB (artigo): https://arxiv.org/pdf/2202.09907
- AG-PT-set: https://zenodo.org/records/10159492
- TART: https://arxiv.org/abs/2510.02597 ; https://arxiv.org/abs/2609.11904
- Noise2Fret: https://arxiv.org/abs/2608.30854
- FretNet: https://arxiv.org/abs/2212.03023 ; https://github.com/cwitkowitz/guitar-transcription-continuous
- Trimplexx: https://github.com/trimplexx/music-transcription
- Reyhanlioglu: https://github.com/ErenReyhanlioglu/Guitar-Transcription
- SynthTab: https://arxiv.org/abs/2309.09085
- Basic Pitch: https://github.com/spotify/basic-pitch ; https://huggingface.co/daserge/basic-pitch-onnx
- SwiftF0: https://github.com/lars76/swift-f0 ; https://arxiv.org/html/2508.18440v1
- PESTO: https://github.com/SonyCSLParis/pesto
- CREPE: https://github.com/marl/crepe
- FCPE: https://arxiv.org/html/2509.15140v1 (via busca)
- Beat This!: https://github.com/CPJKU/beat_this
- MT3: https://github.com/magenta/mt3
- YourMT3: https://github.com/mimbres/YourMT3
- MuScriptor: https://huggingface.co/MuScriptor/muscriptor-large
- ChordMini: https://github.com/ptnghia-j/ChordMini ; https://huggingface.co/musetric/chordmini-onnx (via busca)
- Murgul et al.: https://arxiv.org/html/2508.07973v1
- Avaliação de desempenho: https://arxiv.org/abs/2601.19029 ; https://arxiv.org/html/2608.11026 ; https://arxiv.org/pdf/2409.08795 (via busca)
