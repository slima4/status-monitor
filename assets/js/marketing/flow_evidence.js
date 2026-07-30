import { autoplay, paintTransport, stop, timers, transport, typeOut } from './_figure.js';

/* What a failed step hands back, revealed in the order you would read it.

   The steps run quickly, one fails, and then the evidence arrives a line at a
   time: where the browser ended up, what the page said, what it logged. The URL
   is first because it is usually the whole answer. */

const STEP_MS = 950;
const EV_MS = 1350;

const STEPS = [
  {label: 'fill #username', detail: 'tomsmith', state: 'ok'},
  {label: 'fill #password', detail: '{{login_password}}', state: 'ok'},
  {label: 'click #login > button', detail: 'submitted', state: 'ok'},
  {label: 'assert url /secure', state: 'down', reason: 'url does not contain "/secure"'},
];

const EVIDENCE = [
  {
    label: 'URL',
    value: 'https://the-internet.herokuapp.com/login',
    read: 'never left the login page, so the submit did not take',
  },
  {
    label: 'Title',
    value: 'The Internet',
    read: null,
  },
  {
    label: 'Page text',
    value: 'Your password is invalid!',
    read: 'the app says why, in its own words',
  },
  {
    label: 'Console',
    value: 'nothing logged',
    quiet: true,
    read: 'this app logs nothing, and it did not need to',
  },
];

// Steps first, then the evidence lines, as one timeline.
const ROUNDS = STEPS.length + EVIDENCE.length;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();

  root.classList.add('mk-fig', 'mk-ev');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">a failed run</span>
      <span class="mk-fig__note mk-ev__note"></span>
      <span class="mk-fig__meta">no screenshot: the engine has no renderer</span>
    </div>

    <ol class="mk-ev__steps">
      ${STEPS.map((s, i) => `
        <li class="mk-ev__step" data-step="${i}" data-state="idle">
          <span class="mk-flow__mark" aria-hidden="true"></span>
          <span class="mk-flow__label">${s.label}</span>
          <span class="mk-flow__detail"></span>
          <span class="mk-ev__reason"></span>
        </li>`).join('')}
    </ol>

    <div class="mk-ev__panel" data-open="false">
      <h4 class="mk-fig__colhead">the page when it failed</h4>
      <dl class="mk-ev__list">
        ${EVIDENCE.map((e, i) => `
          <div class="mk-ev__pair" data-ev="${i}" data-state="idle"${e.quiet ? ' data-quiet="true"' : ''}>
            <dt class="mk-ev__key">${e.label}</dt>
            <dd class="mk-ev__val"></dd>
          </div>`).join('')}
      </dl>
    </div>

    <p class="mk-fig__verdict mk-ev__verdict"></p>
    <div class="mk-fig__foot">${transport('Replay the failure', `${ROUNDS} beats`)}</div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const stepRows = [...root.querySelectorAll('.mk-ev__step')];
  const evRows = [...root.querySelectorAll('.mk-ev__pair')];
  const panel = root.querySelector('.mk-ev__panel');
  const noteEl = root.querySelector('.mk-ev__note');
  const verdict = root.querySelector('.mk-ev__verdict');

  const player = {i: -1, playing: false};

  function paint(round){
    player.i = round;
    const stepRound = Math.min(round, STEPS.length - 1);

    stepRows.forEach((el, i) => {
      const s = STEPS[i];
      const reached = round >= 0 && i <= stepRound;
      el.dataset.state = !reached ? 'idle' : i === stepRound && round < STEPS.length
        ? (s.state === 'down' ? 'down' : 'run')
        : s.state;
      el.querySelector('.mk-flow__detail').textContent = reached && s.detail ? s.detail : '';
      el.querySelector('.mk-ev__reason').textContent =
        reached && s.state === 'down' ? s.reason : '';
    });

    const shown = Math.max(0, round - STEPS.length + 1);
    panel.dataset.open = String(shown > 0);
    evRows.forEach((el, i) => {
      const val = el.querySelector('.mk-ev__val');
      el.dataset.state = i < shown ? 'in' : 'idle';
      if (i >= shown){ val.textContent = ''; return; }
      const e = EVIDENCE[i];
      if (i === shown - 1 && player.playing) typeOut(clocks, val, e.value, EV_MS * 0.45);
      else val.textContent = e.value;
    });

    noteEl.textContent = EVIDENCE[shown - 1]?.read || '';

    root.classList.toggle('is-open', round >= STEPS.length - 1);

    const done = round >= ROUNDS - 1;
    verdict.dataset.state = done ? 'down' : '';
    verdict.textContent = done
      ? 'No screenshot needed: the URL says where it stopped, the page says why.'
      : '';

    paintTransport(els, {playing: player.playing, done, round, rounds: ROUNDS, unit: 'beat'});
  }

  function runFrom(round){
    paint(round);
    if (!player.playing) return stop(player, els, {round, rounds: ROUNDS, unit: 'beat'});
    // The last line is still typing itself out, so the transport waits for it.
    if (round >= ROUNDS - 1) {
      clocks.later(EV_MS * 0.5, () => stop(player, els, {round, rounds: ROUNDS, unit: 'beat'}));
      return;
    }
    clocks.later(round < STEPS.length ? STEP_MS : EV_MS, () => runFrom(round + 1));
  }

  // Resuming picks up at the next beat; the one on screen has already shown.
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

document.querySelectorAll('.mk-embed-flow-evidence').forEach(mount);
