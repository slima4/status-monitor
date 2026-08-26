import { autoplay, paintTransport, picker, timers, transport } from './_figure.js';

/* What grace is for.

   A job on a 30 minute schedule does not run every 30 minutes. It runs every
   28, then 34, then 31, because queues back up and locks are held. Grace is the
   slack a monitor allows before it calls that silence an outage, and picking it
   is a straight trade: too little and ordinary jitter pages you, too much and a
   dead job stays green for an hour. The reader turns the dial and both sides of
   that trade move at once. */

const PERIOD_MIN = 30;
const ROUND_MS = 760;

// Eleven real gaps between successes, then the night it stops. None of these is
// a failure: every one of them is a run that finished.
const GAPS = [30, 32, 29, 34, 31, 30, 38, 30, 33, 31, 36];
const ROUNDS = GAPS.length + 1;                    // the last round is the death
const DEAD = GAPS.length;
const WORST = Math.max(...GAPS);

const GRACES = [
  {value: '0', label: '0 min'},
  {value: '5', label: '5 min'},
  {value: '15', label: '15 min'},
  {value: '30', label: '30 min'},
];
const DEFAULT_GRACE = '5';

/** Runs whose gap outlived the window, so the monitor paged for a live job. */
const falsePages = (gaps, grace) => gaps.filter(g => g > PERIOD_MIN + grace).length;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  let grace = Number(DEFAULT_GRACE);

  root.classList.add('mk-fig', 'mk-grace');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">every ${PERIOD_MIN} min</span>
      <span class="mk-fig__note">grace ${picker('How much grace the monitor allows', GRACES, DEFAULT_GRACE)}</span>
      <span class="mk-fig__meta">${GAPS.length} real runs, then the job stops</span>
    </div>

    <div class="mk-fig__lead">
      <p class="mk-fig__score">
        <span class="mk-fig__big">0</span>
        <span class="mk-fig__unit">false pages, for runs that finished</span>
      </p>
      ${transport('Run playback', `${ROUNDS} runs`)}
    </div>

    <div class="mk-grace__flow">
      <p class="mk-fig__colhead">gap between one success and the next</p>
      <div class="mk-grace__chart">
        <span class="mk-grace__bars" aria-hidden="true">${GAPS.map(() =>
          '<i class="mk-grace__bar"></i>').join('')}<i class="mk-grace__bar mk-grace__bar--dead"></i></span>
        <span class="mk-grace__line" aria-hidden="true"><b class="mk-grace__linelab"></b></span>
      </div>
      <p class="mk-fig__legend" aria-hidden="true">
        <span class="mk-fig__key mk-fig__key--ok"></span>inside the window
        <span class="mk-fig__key mk-fig__key--down"></span>past it, and paged
      </p>
    </div>

    <div class="mk-grace__stats">
      <p class="mk-grace__stat"><span>false pages</span><b class="mk-grace__false">0</b></p>
      <p class="mk-grace__stat"><span>a dead job is caught after</span><b class="mk-grace__catch">0m</b></p>
    </div>

    <p class="mk-fig__verdict" role="status" aria-live="polite"></p>
    <p class="mk-fig__caption">The same eleven runs every time, none of them a failure. Grace is the only thing that changes, and it moves both numbers at once: no grace turns ordinary jitter into pages, and generous grace leaves a job that really died looking healthy for an hour. The worst honest gap here is ${WORST} minutes, which is what the window has to clear.</p>`;

  const bars = [...root.querySelectorAll('.mk-grace__bar')];
  const lineEl = root.querySelector('.mk-grace__line');
  const lineLab = root.querySelector('.mk-grace__linelab');
  const pickEl = root.querySelector('.mk-fig__pick');
  const bigEl = root.querySelector('.mk-fig__big');
  const falseEl = root.querySelector('.mk-grace__false');
  const catchEl = root.querySelector('.mk-grace__catch');
  const verdictEl = root.querySelector('.mk-fig__verdict');
  const roundEl = root.querySelector('.mk-fig__round');
  const toggleBtn = root.querySelector('[data-act="toggle"]');
  const prevBtn = root.querySelector('[data-act="prev"]');
  const nextBtn = root.querySelector('[data-act="next"]');

  const player = {i: -1, playing: false};
  const {later, clear: clearTimers} = timers();

  // The tallest thing the chart ever draws, so the scale never moves under the
  // reader when the window grows.
  const CEIL = Math.max(WORST, PERIOD_MIN + 30) + 8;
  root.style.setProperty('--mk-bars', String(ROUNDS));
  bars.forEach((b, i) => b.style.setProperty('--h', String((i === DEAD ? CEIL : GAPS[i]) / CEIL)));

  function verdict(round){
    const bad = falsePages(GAPS, grace);
    const limit = PERIOD_MIN + grace;
    if (round < 0)
      return `A job asked to run every ${PERIOD_MIN} minutes, and the gaps it actually produced. Move the grace dial and watch how many of these honest runs get read as an outage.`;
    if (round < DEAD){
      const g = GAPS[round];
      return g > limit
        ? `Run ${round + 1} took ${g} minutes. The window closed at ${limit}, so the monitor already called this one down and paged somebody. The job was running the whole time.`
        : `Run ${round + 1} took ${g} minutes, inside the ${limit} minute window. Nothing fires, which is the correct answer.`;
    }
    return bad
      ? `The job is dead now, and this window catches that after ${limit} minutes. It also cost ${bad} page${bad === 1 ? '' : 's'} on runs that finished perfectly well, and an alert people have learned to ignore is not much better than no alert.`
      : `The job is dead now, and this window catches that after ${limit} minutes with no false pages along the way. Every honest gap fitted inside it, including the worst one at ${WORST} minutes.`;
  }

  function settle(round){
    const limit = PERIOD_MIN + grace;
    const shown = round < 0 ? [] : GAPS.slice(0, Math.min(round + 1, DEAD));
    const bad = falsePages(shown, grace);
    root.classList.toggle('is-open', round >= DEAD);
    bigEl.textContent = String(bad);
    bigEl.classList.toggle('is-bad', bad > 0);
    falseEl.textContent = String(bad);
    falseEl.classList.toggle('is-bad', bad > 0);
    catchEl.textContent = `${limit}m`;
    lineEl.style.setProperty('--h', String(limit / CEIL));
    lineLab.textContent = `window ${limit}m`;
    verdictEl.textContent = verdict(round);
  }

  function paintBars(round){
    const limit = PERIOD_MIN + grace;
    bars.forEach((b, i) => {
      b.className = i === DEAD ? 'mk-grace__bar mk-grace__bar--dead' : 'mk-grace__bar';
      if (i > round) return;
      b.classList.add('is-in');
      if (i === DEAD) b.classList.add('is-over');
      else if (GAPS[i] > limit) b.classList.add('is-over');
    });
  }

  function controls(){
    paintTransport({toggle: toggleBtn, prev: prevBtn, next: nextBtn, round: roundEl},
      {playing: player.playing, done: player.i >= ROUNDS - 1, round: player.i, rounds: ROUNDS, unit: 'run'});
  }

  function paint(round){
    clearTimers();
    player.i = round;
    paintBars(round);
    settle(round);
    controls();
  }

  function runRound(round){
    paint(round);
    player.playing = true;
    controls();
    later(ROUND_MS, () => {
      if (!player.playing) return;
      if (round >= ROUNDS - 1){ player.playing = false; controls(); return; }
      runRound(round + 1);
    });
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
  // The runs never change. Only the line they are judged against moves.
  pickEl.addEventListener('change', () => {
    grace = Number(pickEl.value);
    // paint() drops the queued run, so the transport has to stop calling itself playing.
    player.playing = false;
    paint(player.i);
  });

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-grace').forEach(mount);
