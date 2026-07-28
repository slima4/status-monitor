import { autoplay, mover, paintTransport, picker, timers, transport } from './_figure.js';

/* The confirmation gate, replayed check by check.

   Three stories run side by side because the gate is about the shape of a run,
   not the number of failures: the blip fails once, the flapping check fails
   three times but never twice in a row, and only the real outage fails back to
   back. The count that decides is the monitor's own setting, so it is the
   control the reader gets to turn. */

const START_MIN = 10 * 60 + 21;                    // clock the replay starts from
const ROUND_MS = 1500;
const STAGGER_MS = 90;                             // the three checks land apart
const HOP = 380;                                   // name → the cell that just landed
const RESET_MS = 420;

const STORIES = [
  {name: 'a blip',        ticks: 'ooxooo'},
  {name: 'flapping',      ticks: 'oxoxox'},
  {name: 'a real outage', ticks: 'ooxxxx'},
];
const ROUNDS = STORIES[0].ticks.length;

// The confirmation counts the monitor form offers, named the way it names them.
const SETTINGS = [1, 2, 3, 5].map(n => ({
  value: String(n),
  label: `${n} fail${n > 1 ? 's' : ''}`,
  need: n,
}));
const DEFAULT_SETTING = '2';

const clock = round => `${String(Math.floor((START_MIN + round) / 60)).padStart(2, '0')}:${String((START_MIN + round) % 60).padStart(2, '0')}`;

// The trailing run of failures, which is the only thing the gate looks at: one
// good check ends the run whatever came before it.
function runAt(ticks, round){
  let n = 0;
  for (let i = round; i >= 0 && ticks[i] === 'x'; i--) n++;
  return n;
}

const LONGEST = Math.max(...STORIES.flatMap(s => s.ticks.split('').map((_, i) => runAt(s.ticks, i))));

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  let setting = SETTINGS.find(s => s.value === DEFAULT_SETTING);
  const need = () => setting.need;

  root.classList.add('mk-fig', 'mk-gate');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">check gate</span>
      <span class="mk-fig__note">counts as down after ${picker('Failures in a row before a region counts as down', SETTINGS, setting.value)} in a row</span>
      <span class="mk-fig__meta"><b class="mk-fig__clock">${clock(0)}</b> · <span class="mk-gate__checks">0 checks</span></span>
    </div>

    <div class="mk-fig__lead">
      <p class="mk-fig__score">
        <span class="mk-fig__big">0</span>
        <span class="mk-fig__unit"></span>
      </p>
      ${transport('Check playback', `${ROUNDS} checks`)}
    </div>

    <div class="mk-gate__flow">
      <p class="mk-fig__colhead">GET api.example.com/health · one region, one check a minute</p>
      <p class="mk-gate__times" aria-hidden="true">
        <span></span>
        <span class="mk-gate__timerow">${STORIES[0].ticks.split('').map((_, i) =>
          `<span class="mk-gate__time">${clock(i)}</span>`).join('')}</span>
        <span></span>
        <span></span>
      </p>
      <div class="mk-gate__lanes">${STORIES.map((s, si) => `
        <div class="mk-gate__lane" data-lane="${si}">
          <span class="mk-gate__name">${s.name}</span>
          <span class="mk-gate__row" aria-hidden="true">${s.ticks.split('').map(() =>
            '<i class="mk-gate__cell"></i>').join('')}</span>
          <span class="mk-fig__pips" aria-hidden="true"></span>
          <span class="mk-gate__state"></span>
        </div>`).join('')}
      </div>
      <p class="mk-fig__legend" aria-hidden="true">
        <span class="mk-fig__key mk-fig__key--ok"></span>passed
        <span class="mk-fig__key mk-fig__key--down"></span>failed
        <span class="mk-fig__key mk-fig__key--idle"></span>not run yet
      </p>
      <p class="mk-fig__verdict" role="status" aria-live="polite"></p>
    </div>

    <p class="mk-fig__caption">The same six checks under three different stories. Turn the setting up and the blip and the flapping check stop counting as down, at the price of a minute or two before a real failure is confirmed. Turn it down to one and every bad check counts, which is how a healthy site pages you at 3 a.m.</p>`;

  const lanes = [...root.querySelectorAll('.mk-gate__lane')];
  const flow = root.querySelector('.mk-gate__flow');
  const pickEl = root.querySelector('.mk-fig__pick');
  const bigEl = root.querySelector('.mk-fig__big');
  const unitEl = root.querySelector('.mk-fig__unit');
  const clockEl = root.querySelector('.mk-fig__clock');
  const checksEl = root.querySelector('.mk-gate__checks');
  const verdictEl = root.querySelector('.mk-fig__verdict');
  const roundEl = root.querySelector('.mk-fig__round');
  const toggleBtn = root.querySelector('[data-act="toggle"]');
  const prevBtn = root.querySelector('[data-act="prev"]');
  const nextBtn = root.querySelector('[data-act="next"]');

  const cells = lanes.map(l => [...l.querySelectorAll('.mk-gate__cell')]);
  const pipRows = lanes.map(l => l.querySelector('.mk-fig__pips'));
  const {place, park, add} = mover(flow);
  const chips = lanes.map(() => add('mk-fig__chip'));

  const player = {i: -1, playing: false};
  // Last check that has actually landed, per story: the tally follows the
  // checks on screen rather than the round the player is on.
  const landed = STORIES.map(() => -1);
  const {later, clear: clearTimers} = timers();

  // One pip per failure the setting asks for, so the row itself says how far a
  // region has to fall before it counts.
  function buildPips(){
    for (const row of pipRows)
      row.innerHTML = Array.from({length: need()}, () => '<i class="mk-fig__pip"></i>').join('');
  }

  function state(run, down){
    return down ? 'counts as down' : run ? `fail ${run} of ${need()}` : 'passing';
  }

  // Paints one story's row up to `round`: results so far, the run it is on, and
  // the word for where that leaves it.
  function paintLane(si, round){
    const lane = lanes[si], ticks = STORIES[si].ticks;
    const run = round < 0 ? 0 : runAt(ticks, round);
    const down = run >= need();
    lane.classList.toggle('is-down', down);
    cells[si].forEach((c, ci) => {
      c.className = 'mk-gate__cell';
      c.textContent = ci > round ? '' : ticks[ci] === 'x' ? '503' : '200';
      if (ci <= round) c.classList.add(ticks[ci] === 'x' ? 'is-fail' : 'is-pass');
    });
    // Clears here rather than only on its own timer: a scrub drops every
    // pending timer, and a flash left behind would never come off.
    pipRows[si].classList.remove('is-reset');
    pipRows[si].querySelectorAll('.mk-fig__pip').forEach((p, pi) => p.classList.toggle('is-lit', pi < run));
    lane.querySelector('.mk-gate__state').textContent = round < 0 ? 'idle' : state(run, down);
  }

  function verdict(round, marks){
    if (round < 0) return `Three stories, one region each, one check a minute. Press play and watch what a run of ${need()} does to each of them.`;
    if (round < ROUNDS - 1) return `${marks} ${marks === 1 ? 'time' : 'times'} a region counts as down so far.`;
    const late = need() === 1 ? 'the moment it fails' : `${need() - 1} minute${need() > 2 ? 's' : ''} after its first failure`;
    if (!marks)
      return `All ${ROUNDS} checks are in and nothing counts as down. The longest run is ${LONGEST} failures and this setting needs ${need()}, so the real outage is still waiting to be confirmed.`;
    if (marks === 1)
      return `All ${ROUNDS} checks are in. One region counted as down once, ${late}, and it was the sustained failure. The blip resets on its next good check, and flapping never fails twice in a row.`;
    return `All ${ROUNDS} checks are in and a region counted as down ${marks} times, though only one of them was a real outage. At ${setting.label} the other ${marks - 1} are a blip and a flapping check.`;
  }

  // Every check where a story crossed from passing to counting as down. A
  // flapping check crosses more than once, which is what a low setting costs.
  function marks(){
    let n = 0;
    STORIES.forEach((s, si) => {
      for (let i = 0; i <= landed[si]; i++) if (runAt(s.ticks, i) === need()) n++;
    });
    return n;
  }

  function settle(round){
    const n = marks();
    root.classList.toggle('is-open', n > 0);
    bigEl.textContent = String(n);
    unitEl.textContent = `${n === 1 ? 'time' : 'times'} a region counts as down across ${ROUNDS} checks`;
    verdictEl.textContent = verdict(round, n);
  }

  function controls(){
    paintTransport({toggle: toggleBtn, prev: prevBtn, next: nextBtn, round: roundEl},
      {playing: player.playing, done: player.i >= ROUNDS - 1, round: player.i, rounds: ROUNDS, unit: 'check'});
  }

  // Full state for a check, with nothing in the air: scrubbing, pausing, rest.
  function paint(round){
    clearTimers();
    player.i = round;
    chips.forEach(park);
    // Scrubbing lands everything at once, so nothing is left in the air.
    landed.fill(round);
    lanes.forEach((_, si) => paintLane(si, round));
    settle(round);
    clockEl.textContent = clock(Math.max(round, 0));
    checksEl.textContent = `${round < 0 ? 0 : STORIES.length * (round + 1)} checks`;
    controls();
  }

  // One check, run by every story at once.
  function runRound(round){
    player.i = round;
    clockEl.textContent = clock(round);
    controls();
    lanes.forEach((lane, si) => {
      const d = si * STAGGER_MS, chip = chips[si];
      later(d, () => {
        place(chip, lane.querySelector('.mk-gate__name'), 0);
        chip.classList.add('is-live');
        later(20, () => place(chip, cells[si][round], HOP));
      });
      later(d + HOP + 40, () => {
        park(chip);
        land(si, round);
      });
    });
    later(ROUND_MS, () => {
      if (!player.playing) return;
      if (round >= ROUNDS - 1){ player.playing = false; controls(); return; }
      runRound(round + 1);
    });
  }

  // A check lands. A good one after a bad run is the moment worth seeing, so
  // the counter flashes as it clears.
  function land(si, round){
    const ticks = STORIES[si].ticks;
    const broke = ticks[round] === 'o' && round > 0 && runAt(ticks, round - 1) > 0;
    landed[si] = round;
    paintLane(si, round);
    if (broke){
      pipRows[si].classList.add('is-reset');
      later(RESET_MS, () => pipRows[si].classList.remove('is-reset'));
    }
    settle(round);
    checksEl.textContent = `${STORIES.length * round + si + 1} checks`;
  }

  function play(){
    if (player.i >= ROUNDS - 1) paint(-1);
    player.playing = true;
    if (calm.matches){ player.playing = false; paint(ROUNDS - 1); return; }
    runRound(player.i < 0 ? 0 : player.i);
  }

  function pause(){ player.playing = false; paint(player.i); }

  function step(to){
    player.playing = false;
    paint(Math.min(ROUNDS - 1, Math.max(0, to)));
  }

  toggleBtn.addEventListener('click', () => player.playing ? pause() : play());
  prevBtn.addEventListener('click', () => step(player.i - 1));
  nextBtn.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));
  // Changing the setting re-reads the checks already on screen: same run, new
  // line to cross.
  pickEl.addEventListener('change', () => {
    setting = SETTINGS.find(s => s.value === pickEl.value);
    buildPips();
    lanes.forEach((_, si) => paintLane(si, landed[si]));
    settle(player.i);
  });

  buildPips();
  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-gate').forEach(mount);
