/* =====================================================================
   Teoria: notas, intervalos (T, 3m, 3M, 4J, 5J...), fases e acordes
   ===================================================================== */
var NOTA_ABC = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
var NOTA_ABC_B = ['C', 'Db', 'D', 'Eb', 'E', 'F', 'Gb', 'G', 'Ab', 'A', 'Bb', 'B'];
var NOTA_PT = ['Dó', 'Dó#', 'Ré', 'Ré#', 'Mi', 'Fá', 'Fá#', 'Sol', 'Sol#', 'Lá', 'Lá#', 'Si'];
var NOTA_PT_B = ['Dó', 'Réb', 'Ré', 'Mib', 'Mi', 'Fá', 'Solb', 'Sol', 'Láb', 'Lá', 'Sib', 'Si'];

// intervalos a partir da tônica, na notação brasileira (o "negócio dos jotas")
var IV = ['T', '2m', '2M', '3m', '3M', '4J', '4A', '5J', '6m', '6M', '7m', '7M'];
var IV_NOME = ['tônica', 'segunda menor', 'segunda maior', 'terça menor', 'terça maior', 'quarta justa',
  'quarta aumentada', 'quinta justa', 'sexta menor', 'sexta maior', 'sétima menor', 'sétima maior'];
// uma cor por intervalo: a tônica é sempre vermelha, a 5J sempre azul, em qualquer tom
var IV_COR = ['#ff5964', '#ff8552', '#ffa630', '#ffd23f', '#c8e64b', '#4ade80', '#2dd4bf', '#38bdf8', '#7c93ff', '#a78bfa', '#e879f9', '#f472b6'];

var CORDAS = [40, 45, 50, 55, 59, 64];                 // 6ª (Mi grave) ... 1ª (Mi agudo)
var CORDA_NOME = ['6ª', '5ª', '4ª', '3ª', '2ª', '1ª'];

function mtof(m) { return 440 * Math.pow(2, (m - 69) / 12); }
function abc(pc, bemol) { return (bemol ? NOTA_ABC_B : NOTA_ABC)[((pc % 12) + 12) % 12]; }
function pt(pc, bemol) { return (bemol ? NOTA_PT_B : NOTA_PT)[((pc % 12) + 12) % 12]; }
function usaBemol(tonica, menor) { return (menor ? [2, 7, 0, 5, 10, 3] : [5, 10, 3, 8, 1]).indexOf(tonica) >= 0; }

/* Fases do Caça-Notas. pos = [corda (0 = 6ª), casa], na ordem em que as notas são liberadas. */
var FASES = [
  { id: 'soltas', nome: 'Cordas soltas', curto: 'Soltas', tonica: 4, de: 0, ate: 3,
    pos: [[0, 0], [1, 0], [2, 0], [3, 0], [4, 0], [5, 0]],
    dica: 'Nenhum dedo no braço. Da mais grossa para a mais fina: Mi, Lá, Ré, Sol, Si, Mi.' },
  { id: 'em-penta', nome: 'Pentatônica de Mi menor', curto: 'Mi menor', tonica: 4, de: 0, ate: 3,
    pos: [[0, 0], [0, 3], [1, 0], [1, 2], [2, 0], [2, 2], [3, 0], [3, 2], [4, 0], [4, 3], [5, 0], [5, 3]],
    dica: 'Cinco notas (T, 3m, 4J, 5J, 7m) que se repetem pelas seis cordas. Usa cordas soltas: é a escala mais fácil de começar.' },
  { id: 'am-penta', nome: 'Pentatônica de Lá menor', curto: 'Lá menor', tonica: 9, de: 5, ate: 8,
    pos: [[0, 5], [0, 8], [1, 5], [1, 7], [2, 5], [2, 7], [3, 5], [3, 7], [4, 5], [4, 8], [5, 5], [5, 8]],
    dica: 'O desenho mais famoso do violão e da guitarra: dedo 1 na casa 5. Os mesmos intervalos de Mi menor, cinco casas acima.' },
  { id: 'c-maior', nome: 'Escala de Dó maior', curto: 'Dó maior', tonica: 0, de: 0, ate: 3,
    pos: [[1, 3], [2, 0], [2, 2], [2, 3], [3, 0], [3, 2], [4, 0], [4, 1], [4, 3], [5, 0], [5, 1], [5, 3], [0, 0], [0, 1], [0, 3], [1, 0], [1, 2]],
    dica: 'As sete notas naturais (T, 2M, 3M, 4J, 5J, 6M, 7M). É dela que saem os acordes do Monta-Acorde.' }
];
function notasDaFase(fase) {
  return fase.pos.map(function (p) {
    var midi = CORDAS[p[0]] + p[1], pc = midi % 12;
    return { s: p[0], f: p[1], midi: midi, pc: pc, iv: (pc - fase.tonica + 12) % 12 };
  });
}

/* Campo harmônico de Dó maior. shape = casa em cada corda (6ª -> 1ª), -1 = não toca. */
var ESCALA_C = [0, 2, 4, 5, 7, 9, 11];
var ACORDES = [
  { grau: 'I', nome: 'C', ext: 'Dó maior', raiz: 0, menor: false, shape: [-1, 3, 2, 0, 1, 0] },
  { grau: 'ii', nome: 'Dm', ext: 'Ré menor', raiz: 2, menor: true, shape: [-1, -1, 0, 2, 3, 1] },
  { grau: 'iii', nome: 'Em', ext: 'Mi menor', raiz: 4, menor: true, shape: [0, 2, 2, 0, 0, 0] },
  { grau: 'IV', nome: 'F', ext: 'Fá maior', raiz: 5, menor: false, shape: [-1, -1, 3, 2, 1, 1] },
  { grau: 'V', nome: 'G', ext: 'Sol maior', raiz: 7, menor: false, shape: [3, 2, 0, 0, 0, 3] },
  { grau: 'vi', nome: 'Am', ext: 'Lá menor', raiz: 9, menor: true, shape: [-1, 0, 2, 2, 1, 0] }
];
function triade(ac) { return [ac.raiz, (ac.raiz + (ac.menor ? 3 : 4)) % 12, (ac.raiz + 7) % 12]; }
function papeis(ac) { return ['T', ac.menor ? '3m' : '3M', '5J']; }
function coresPapel(ac) { return [IV_COR[0], IV_COR[ac.menor ? 3 : 4], IV_COR[7]]; }

/* Graus do campo harmônico maior (para a base do Jam) */
var GRAUS = [
  { g: 'I', st: 0, menor: false }, { g: 'ii', st: 2, menor: true }, { g: 'iii', st: 4, menor: true },
  { g: 'IV', st: 5, menor: false }, { g: 'V', st: 7, menor: false }, { g: 'vi', st: 9, menor: true }
];
function pentaDoTom(tonica, menor) {                    // conjunto de 5 classes de nota
  var tm = menor ? tonica : (tonica + 9) % 12;
  return [tm, (tm + 3) % 12, (tm + 5) % 12, (tm + 7) % 12, (tm + 10) % 12];
}
function janelaDoTom(tonica, menor) {                   // 4 casas onde a pentatônica cai bem
  var tm = menor ? tonica : (tonica + 9) % 12, r = (tm - 4 + 12) % 12;
  var ini = r <= 5 ? r : r - 5;
  return { de: ini, ate: ini + 3 };
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { FASES: FASES, ACORDES: ACORDES, notasDaFase: notasDaFase, triade: triade, CORDAS: CORDAS, pentaDoTom: pentaDoTom, janelaDoTom: janelaDoTom, abc: abc, pt: pt };
}
