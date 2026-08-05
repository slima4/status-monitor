import { autoplay, choices, mix, paintTransport, stop, timers, transport } from './_figure.js';

/* Where a fleet of monitors lands inside one 60-second interval.

   Each bar is one second of the interval and its height is how many monitors
   fire in it. The three offset rules are the three answers people actually
   reach for, and the fleet churns underneath them for sixteen rounds: counting
   spreads as evenly as hashing on any single frame, and pays for it on every
   frame after. Only the running total shows that. */

const ROUND_MS = 550;
const INTERVAL_S = 60;

/* A fleet that grows the way a real one does, with restarts in it. A restart
   changes nothing about the monitors: the config query carries no ORDER BY, so
   the same rows can simply come back in a different order. */
const EVENTS = [
  {kind: 'boot', n: 800},
  {kind: 'add', n: 120},
  {kind: 'restart'},
  {kind: 'add', n: 260},
  {kind: 'remove', n: 90},
  {kind: 'add', n: 40},
  {kind: 'restart'},
  {kind: 'remove', n: 200},
  {kind: 'add', n: 310},
  {kind: 'add', n: 75},
  {kind: 'restart'},
  {kind: 'remove', n: 150},
  {kind: 'add', n: 90},
  {kind: 'restart'},
  {kind: 'add', n: 180},
  {kind: 'remove', n: 60},
];

/* Only the two rounds whose cause is not already in the label: an add or a
   remove says what it did, a boot and a restart do not. */
const NOTES = {
  boot: 'every monitor scheduled at once',
  restart: 'same fleet, rows reloaded in a different order',
};

const label = e => e.kind === 'boot' ? 'boot'
  : e.kind === 'restart' ? 'restart'
  : `${e.kind === 'add' ? '+' : '-'}${e.n}`;

const RULES = [
  {
    value: 'none', label: 'no offset',
    why: 'Every monitor is due on the same second, forever. The fleet spends one second at full tilt and 59 idle.',
  },
  {
    value: 'counter', label: 'count around the circle',
    why: 'Spreads perfectly on any one frame, because every slot is derived from the fleet size and the row order. Both of those move.',
  },
  {
    value: 'hashed', label: 'hash the id',
    why: 'Each monitor works out its own slot from its own id alone. The spread is lumpier than counting, and in exchange nothing else in the fleet can ever move it.',
  },
];
const DEFAULT_RULE = 'hashed';

/** Slot each monitor fires on, under one rule, in fleet order. */
function slots(ids, rule){
  if (rule === 'none') return ids.map(() => 0);
  if (rule === 'counter') return ids.map((_, i) => Math.floor(i * INTERVAL_S / ids.length) % INTERVAL_S);
  return ids.map(id => id % INTERVAL_S);
}

/* One pass over the whole run, so a scrub or a rule change is a lookup rather
   than a rebuild, and the running total is summed once instead of per frame. */
function buildFrames(rule){
  const frames = [];
  let ids = [];
  let next = 0;
  let prev = null;
  let total = 0;

  for (const ev of EVENTS){
    if (ev.kind === 'boot' || ev.kind === 'add'){
      for (let i = 0; i < ev.n; i++) ids.push(mix(next++));
    } else if (ev.kind === 'remove'){
      // One customer's monitors, made together and deleted together, so the
      // block is contiguous. Thinning evenly instead would leave every survivor
      // at the same proportional position, the one shape counting survives.
      ids.splice(Math.floor(ids.length * 0.25), ev.n);
    } else {
      // Deterministic, so the replay lands on the same picture every time.
      ids = ids.slice().sort((a, b) => mix(a ^ 0x5bf03635) - mix(b ^ 0x5bf03635));
    }

    const list = slots(ids, rule);
    const map = new Map();
    ids.forEach((id, i) => map.set(id, list[i]));

    const bins = new Array(INTERVAL_S).fill(0);
    for (const s of list) bins[s]++;

    // Only monitors present in both frames count: one just added never had a
    // slot to move from, and one just removed has nowhere to move to.
    let moved = 0;
    if (prev) for (const [id, slot] of map) if (prev.has(id) && prev.get(id) !== slot) moved++;
    total += moved;

    frames.push({
      ev, size: ids.length, bins, moved, total,
      peak: Math.max(...bins),
      idle: bins.filter(n => n === 0).length,
    });
    prev = map;
  }
  return frames;
}

const ROUNDS = EVENTS.length;
const FLEET_START = EVENTS[0].n;
const FLEET_END = EVENTS.reduce((n, e) => n + (e.n || 0) * (e.kind === 'remove' ? -1 : 1), 0);

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();
  let rule = DEFAULT_RULE;
  let frames = buildFrames(rule);

  root.classList.add('mk-fig', 'mk-stagger');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">one 60-second interval</span>
      <span class="mk-fig__note">how each monitor picks its second</span>
    </div>

    ${choices('How the offset is picked', RULES, DEFAULT_RULE)}

    <p class="mk-stagger__why"></p>

    <div class="mk-stagger__plot">
      <div class="mk-stagger__bars" role="img"></div>
      <div class="mk-stagger__axis" aria-hidden="true">
        <span>0s</span><span>30s</span><span>60s</span>
      </div>
    </div>

    <dl class="mk-stagger__stats">
      <div><dt>monitors</dt><dd data-stat="fleet">0</dd></div>
      <div><dt>busiest second</dt><dd data-stat="peak">0</dd></div>
      <div><dt>idle seconds</dt><dd data-stat="idle">0</dd></div>
      <div><dt>slot changes so far</dt><dd data-stat="total">0</dd></div>
    </dl>

    <p class="mk-fig__legend" aria-hidden="true">
      <span class="mk-fig__key mk-fig__key--ok"></span>monitors due that second
      <span class="mk-fig__key mk-fig__key--setup"></span>three times the even share
      <span class="mk-fig__key mk-fig__key--idle"></span>nothing scheduled
    </p>

    <p class="mk-fig__verdict mk-stagger__verdict" role="status" aria-live="polite"></p>

    <div class="mk-fig__foot mk-fig__foot--split">
      ${transport('Replay the fleet churning', `${ROUNDS} rounds`)}
      <span class="mk-fig__prov">${FLEET_START.toLocaleString()} to ${FLEET_END.toLocaleString()} monitors · ${INTERVAL_S}s interval · modelled, not measured</span>
    </div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const pick = root.querySelector('.mk-fig__opts');
  const barHost = root.querySelector('.mk-stagger__bars');
  const why = root.querySelector('.mk-stagger__why');
  const verdict = root.querySelector('.mk-stagger__verdict');
  const stat = name => root.querySelector(`[data-stat="${name}"]`);

  const bars = Array.from({length: INTERVAL_S}, () => {
    const el = document.createElement('i');
    el.className = 'mk-stagger__bar';
    barHost.appendChild(el);
    return el;
  });

  const player = {i: -1, playing: false};

  function paint(round){
    player.i = round;
    const f = frames[Math.max(0, round)];

    bars.forEach((el, i) => {
      el.style.setProperty('--fill', `${f.bins[i] / f.peak}`);
      el.dataset.state = f.bins[i] === 0 ? 'idle'
        : f.bins[i] > f.size / INTERVAL_S * 3 ? 'hot' : 'ok';
      el.title = `second ${i} · ${f.bins[i].toLocaleString()} of ${f.size.toLocaleString()} monitors due`;
    });
    barHost.setAttribute(
      'aria-label',
      `${f.size} monitors across ${INTERVAL_S} seconds. Busiest second fires ${f.peak}. ${f.idle} seconds fire nothing.`,
    );

    stat('fleet').textContent = f.size.toLocaleString();
    stat('peak').textContent = f.peak.toLocaleString();
    stat('idle').textContent = `${f.idle} of ${INTERVAL_S}`;
    stat('total').textContent = round < 0 ? '0' : f.total.toLocaleString();

    why.textContent = RULES.find(r => r.value === rule).why;

    // A restart carries its cause, because nothing about the fleet changed and
    // a bare count of monitors that moved would otherwise read as a bug.
    const cause = f.ev.kind === 'restart' ? `${NOTES.restart}. ` : '';
    const effect = rule === 'none' ? `all ${f.size.toLocaleString()} due on one second`
      : f.moved ? `${f.moved.toLocaleString()} changed second`
      : 'nothing changed second';
    verdict.textContent = round < 0 ? ''
      : round === 0 ? `${label(f.ev)} · ${NOTES.boot}`
      : `${label(f.ev)} · ${cause}${effect}`;

    paintTransport(els, {playing: player.playing, done: round >= ROUNDS - 1, round, rounds: ROUNDS});
  }

  function runFrom(round){
    paint(round);
    if (!player.playing || round >= ROUNDS - 1) return stop(player, els, {round, rounds: ROUNDS});
    clocks.later(ROUND_MS, () => runFrom(round + 1));
  }

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

  // The rules only differ once the fleet has moved, so switching replays from
  // the top rather than leaving the reader on a frame that proves nothing.
  pick.addEventListener('change', e => {
    rule = e.target.value;
    frames = buildFrames(rule);
    clocks.clear();
    player.playing = false;
    if (calm.matches) paint(ROUNDS - 1);
    else { paint(-1); play(); }
  });

  els.toggle.addEventListener('click', () => player.playing ? pause() : play());
  els.prev.addEventListener('click', () => step(player.i - 1));
  els.next.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));

  paint(calm.matches ? ROUNDS - 1 : -1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-stagger').forEach(mount);
