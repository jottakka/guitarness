# Pesquisa: detecção estrita de acorde, corda e batida

Data: 16/11/2026. Pesquisa assistida, limitada a cerca de 7 buscas; muitos números não foram verificados. Este texto é pesquisa, não decisão.

## 1. Transcrição de corda e casa a partir de áudio mono

- TabCNN (Wiggins e Kim, ISMIR 2019): CNN sobre CQT, treinada no GuitarSet, com softmax por corda sobre as casas. Pequena, por quadro, viável em tempo real.
- FretNet (Cwitkowitz et al., ICASSP 2023, arXiv 2212.03023): espinha dorsal do TabCNN mais contorno de altura por corda. F1 de tablatura de cerca de 0,75 a 0,85 no GuitarSet (NÃO VERIFICADO).
- GAPS (Riley et al., ISMIR 2024, arXiv 2408.08653): 14 h de violão clássico, só MIDI por nota (sem rótulo de corda). Afirma estado da arte em transcrição de notas. Licença não informada.
- Mais recentes (NoodleGP 2025, Fretting-Transformer 2025 só simbólico, difusão para tablatura 2026, NÃO VERIFICADO): nada claramente melhor que seja pequeno e em fluxo contínuo.
- Conclusão: modelos de tablatura só com áudio não são precisos o bastante, sozinhos, para aprovar ou reprovar cada corda de forma estrita. Os rasgueados completos (4 a 6 cordas) são o caso mais difícil.

## 2. Conjuntos de dados

- GuitarSet: 360 trechos, captador hexafônico, rótulos por corda. O GitHub é MIT; a licença do Zenodo é separada. Tem acompanhamento e rasgueados, sem rótulo de direção.
- EGDB (2022): guitarra elétrica, hexafônico, 240 tablaturas, reamplificado. Licença não encontrada.
- IDMT-SMT-Guitar (Zenodo 7544110): notas, acordes e técnicas. Licença não verificada.
- Guitar-TECHS (ICASSP 2025): CC BY 4.0 (resultado de busca).
- Direção do rasgueado: Murgul, Schimper e Heizmann (ISMIR 2025, arXiv 2508.07973): cerca de 90 min reais (rotulados por IMU) e cerca de 4 h sintéticas; publicação e licença não informadas.

## 3. Direção do rasgueado

- Murgul et al. 2025 (CRNN, biGRU, não causal): F1 de ataque de rasgueado 92,75% (misto) e 85,06% só com celular. F1 de direção: microfone de celular, 85,5% para baixo e 79,0% para cima; captador, 87,9 e 84,9; captador mais IMU, 90,0 e 88,7. Acurácia maior/menor de acorde no microfone: 90,1%. Latência e parâmetros não foram informados; seria preciso reescrever de forma causal.
- Pistas de áudio: ordem de ataque das cordas (grave para agudo no golpe para baixo) e tendência de brilho. São pistas fracas, daí os cerca de 79% para cima.
- Câmera: não há acurácia rigorosa publicada de subir/descer com MediaPipe; abordagens amadoras usam a velocidade vertical da mão. Latência de cerca de 30 a 60 ms mais a inferência (NÃO VERIFICADO). Fisicamente sem ambiguidade, então é o melhor desempate de direção.

## 4. Teste de hipótese com DSP clássico (raciocínio próprio, pouca literatura direta)

- Barbancho et al., IEEE TASLP 2012 (tablatura de acordes por inarmonicidade); EPFL 2017, dedilhado de nota única por inarmonicidade (12,3% de erro); NMF de piano com inarmonicidade (hal-00727398).
- Camadas: pente harmônico por corda esperada, com modelos calibrados pelo jogador; NMF ou NNLS com modelos fixos (um por corda e casa, aprendidos na calibração); grave mais croma para identidade; inarmonicidade e timbre para separar a mesma altura em cordas diferentes.
- Corda a mais: modelos para toda corda não esperada; sinalizar ativação significativa; priorizar as cordas graves sob o baixo esperado (Mi grave sobre o Ré).
- Corda ausente ou abafada: f0 e parciais esperados ausentes ou com decaimento muito curto.
- Armadilhas: parciais compartilhados (oitava e quinta), ambiguidade de oitava, ressonância por simpatia (falsos extras), decaimento das vizinhas, vazamento, reverberação, e cordas mortas ainda produzem um "toc". Julgar extras pela energia no ataque em relação ao rasgueado, não pelo regime estável; usar janelas alinhadas no tempo e cientes do decaimento.

## 5. Aplicativos comerciais

- Rocksmith: o desenvolvedor diz que a detecção de acordes é "muito precisa"; o diário do Rocksmith+ (fev. 2022) dá peso à terça e à fundamental do baixo; havia problemas com maior e menor em 2014. Jogadores relatam que os riffs são lenientes.
- Yousician: marca a corda de vermelho quando ela não é ouvida no acorde; o fórum relata leituras erradas em músicas rápidas.
- Fender Play e Simply Guitar: sem dados públicos.

## Abordagem recomendada

1. Camada 1, teste de hipótese com DSP (tempo real): o ataque abre uma janela; ajustar modelos calibrados por corda; calcular escores presente, ausente e extra por corda contra a forma esperada; razões de verossimilhança calibradas. A acurácia é desconhecida e precisa ser medida.
2. Camada 2, rede neural pequena e causal por corda (estilo TabCNN ou FretNet, ou CRNN causal, com 6 cabeças de abafada e casa), treinada no GuitarSet, no EGDB e em rasgueados sintéticos das nossas formas; serve de segunda opinião e desempate.
3. Camada 3, câmera opcional para subir/descer e tempo.
4. Fusão: reprovar uma corda só quando o DSP e a rede concordam, ou quando o DSP é fortemente negativo.

Gravar um conjunto de validação próprio para medir falsa reprovação e falsa aprovação por acorde.

Fontes: arxiv.org/html/2508.07973v1, arxiv.org/abs/2212.03023, arxiv.org/abs/2408.08653, arxiv.org/pdf/2202.09907, github.com/marl/GuitarSet, zenodo.org/records/7544110, Yousician support 202893201, diário de desenvolvimento do Rocksmith+ (Ubisoft, fev. 2022), hal-00727398, infoscience.epfl.ch/record/228929.
