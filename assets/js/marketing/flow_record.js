import { autoplay, paintTransport, stop, timers, transport } from './_figure.js';

/* What the importer does to a recording, one recorded action at a time.

   The left column is what Chrome wrote down on the-internet.herokuapp.com; the
   right is what the monitor keeps. Each action flies across and rewrites itself
   on the way. They are deliberately not the same length: the focus click before
   typing folds away, and the password arrives as a reference rather than the
   thing that was typed. */

const ROUND_MS = 1800;
const HOP_MS = 700;
const GLYPHS = '#$%&*+/<=>?@[]^~';

// `emits` is the row this recorded action produces; `folds` means it also
// strikes out the row before it, which the import discards.
const ACTIONS = [
  {
    recorded: 'click   #username',
    emits: {op: 'click', body: '#username'},
    note: null,
  },
  {
    recorded: 'change  #username  "tomsmith"',
    emits: {op: 'fill', body: '#username = tomsmith'},
    folds: true,
    note: 'the focus click folds into the typing',
  },
  {
    recorded: 'change  #password  "SuperSecretPassword!"',
    emits: {op: 'fill', body: '#password = {{login_password}}'},
    secret: true,
    note: 'recorded value dropped, pointed at a secret',
  },
  {
    recorded: 'click   #login > button > i',
    emits: {op: 'click', body: '#login > button'},
    note: 'retargeted to the control the click activates',
  },
  {
    recorded: 'navigate  /secure',
    emits: {op: 'assert_url', body: '/secure'},
    note: 'the navigation becomes the success signal',
  },
];

const ROUNDS = ACTIONS.length;
const line = (s) => `${s.op}  ${s.body}`;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();

  root.classList.add('mk-fig', 'mk-rec');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">Recorder export → steps</span>
      <span class="mk-fig__note mk-rec__note"></span>
      <span class="mk-fig__meta"><span class="mk-rec__count">0 steps</span></span>
    </div>

    <div class="mk-rec__pair">
      <section class="mk-rec__side">
        <h4 class="mk-fig__colhead">what Chrome recorded</h4>
        <ol class="mk-rec__list" data-list="in">
          ${ACTIONS.map((a, i) => `
            <li class="mk-rec__row" data-row="${i}" data-state="idle">
              <span class="mk-rec__text">${a.recorded}</span>
            </li>`).join('')}
        </ol>
      </section>

      <section class="mk-rec__side">
        <h4 class="mk-fig__colhead">what the monitor keeps</h4>
        <ol class="mk-rec__list" data-list="out"></ol>
      </section>

      <span class="mk-rec__fly" aria-hidden="true"></span>
    </div>

    <p class="mk-fig__verdict mk-rec__verdict"></p>
    <div class="mk-fig__foot">${transport('Replay the import', `${ROUNDS} actions`)}</div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const pair = root.querySelector('.mk-rec__pair');
  const inRows = [...root.querySelectorAll('[data-list="in"] .mk-rec__row')];
  const outList = root.querySelector('[data-list="out"]');
  const flyer = root.querySelector('.mk-rec__fly');
  const noteEl = root.querySelector('.mk-rec__note');
  const countEl = root.querySelector('.mk-rec__count');
  const verdict = root.querySelector('.mk-rec__verdict');

  const player = {i: -1, playing: false};

  // Rebuilt each frame so scrubbing backwards lands on the list playing
  // forwards would have produced.
  function frame(round){
    const out = [];
    for (let i = 0; i <= round; i++){
      const a = ACTIONS[i];
      if (a.folds && out.length) out[out.length - 1].folded = true;
      out.push({...a.emits, secret: !!a.secret, from: i});
    }
    return out;
  }

  // Hidden in the markup, not after insertion: hiding it later transitions it
  // out from full opacity, which reads as a blink.
  function row(s, round, flying){
    const state = s.folded ? 'folded' : s.from === round ? 'live' : 'done';
    const waiting = flying && state === 'live' ? ' is-waiting' : '';
    return `
      <li class="mk-rec__row${waiting}" data-state="${state}"${s.secret ? ' data-secret="true"' : ''}>
        <span class="mk-rec__op">${s.op}</span>
        <span class="mk-rec__text">${s.body}</span>
      </li>`;
  }

  // The landing row is held blank until the line arrives, or the same text
  // would sit in two places at once.
  function fly(from, to, was, becomes, secret){
    const h = pair.getBoundingClientRect();
    const a = from.getBoundingClientRect();
    const b = to.getBoundingClientRect();

    flyer.classList.toggle('is-secret', secret);
    flyer.textContent = was;
    flyer.style.transition = 'none';
    flyer.style.width = `${a.width}px`;
    flyer.style.transform = `translate(${a.left - h.left}px, ${a.top - h.top}px)`;
    flyer.classList.add('is-live');

    clocks.later(40, () => {
      // Opacity restated, or the `none` above cuts the flyer off the instant it
      // lands instead of handing over to the row beneath.
      flyer.style.transition =
        `transform ${HOP_MS}ms cubic-bezier(.4,0,.1,1), width ${HOP_MS}ms cubic-bezier(.4,0,.1,1), opacity 180ms linear`;
      flyer.style.width = `${b.width}px`;
      flyer.style.transform = `translate(${b.left - h.left}px, ${b.top - h.top}px)`;
      cipher(was, becomes, HOP_MS);
    });
    clocks.later(HOP_MS + 60, () => {
      to.classList.remove('is-waiting');
      flyer.classList.remove('is-live');
    });
  }

  // Glyphs are picked from the column index, so a replay looks the same twice.
  function cipher(was, becomes, ms){
    const frames = 16;
    for (let f = 1; f <= frames; f++){
      clocks.later((ms / frames) * f, () => {
        const p = f / frames;
        const len = Math.round(was.length + (becomes.length - was.length) * p);
        const kept = Math.round(len * p);
        let out = '';
        for (let i = 0; i < len; i++){
          out += i < kept ? (becomes[i] ?? '') : GLYPHS[(i * 5 + f * 3) % GLYPHS.length];
        }
        flyer.textContent = out;
      });
    }
    clocks.later(ms, () => { flyer.textContent = becomes; });
  }

  function showVerdict(){
    verdict.dataset.state = 'ok';
    verdict.textContent = '5 recorded actions became 4 steps, and the password never reached the config.';
  }

  function paint(round){
    player.i = round;

    inRows.forEach((el, i) => {
      el.dataset.state = round < 0 || i > round ? 'idle' : i === round ? 'live' : 'done';
    });

    const flying = player.playing && round >= 0;
    const out = frame(round);
    outList.innerHTML = out.map((s) => row(s, round, flying)).join('');

    noteEl.textContent = (round >= 0 && ACTIONS[round].note) || '';
    const kept = out.filter((s) => !s.folded).length;
    countEl.textContent = `${kept} step${kept === 1 ? '' : 's'}`;

    const landed = outList.querySelector('[data-state="live"]');
    if (player.playing && round >= 0 && landed){
      const a = ACTIONS[round];
      fly(inRows[round], landed, a.recorded, line(a.emits), !!a.secret);
    } else {
      flyer.classList.remove('is-live');
    }

    const done = round >= ROUNDS - 1;
    // The verdict counts the steps, so it waits for the last one to land.
    if (done && player.playing) clocks.later(HOP_MS + 320, showVerdict);
    else if (done) showVerdict();
    else { verdict.dataset.state = ''; verdict.textContent = ''; }

    paintTransport(els, {playing: player.playing, done, round, rounds: ROUNDS, unit: 'action'});
  }

  function runFrom(round){
    paint(round);
    if (!player.playing) return stop(player, els, {round, rounds: ROUNDS, unit: 'action'});
    // The last action is still in the air, so the transport waits for it.
    if (round >= ROUNDS - 1) {
      clocks.later(HOP_MS + 120, () => stop(player, els, {round, rounds: ROUNDS, unit: 'action'}));
      return;
    }
    clocks.later(ROUND_MS, () => runFrom(round + 1));
  }

  // Resuming picks up at the next action; the one on screen has already flown.
  function play(){
    clocks.clear();
    if (player.i >= ROUNDS - 1) paint(-1);
    player.playing = true;
    if (calm.matches){ player.playing = false; paint(ROUNDS - 1); return; }
    runFrom(player.i + 1);
  }

  function pause(){ player.playing = false; clocks.clear(); paint(player.i); }

  function step(to){
    player.playing = false;
    clocks.clear();
    paint(Math.min(ROUNDS - 1, Math.max(0, to)));
  }

  els.toggle.addEventListener('click', () => player.playing ? pause() : play());
  els.prev.addEventListener('click', () => step(player.i - 1));
  els.next.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-flow-record').forEach(mount);
