import { autoplay, choices, paintTransport, stop, timers, transport, typeOut } from './_figure.js';

/* One login journey, run in both places at once.

   The left column is the suite in CI and it passes under every fault, because
   none of these faults can exist in a pipeline. The right column is the same
   five steps against production, where they can. Picking a fault moves where
   production dies while CI marches on to green, which is the argument the
   prose can only assert. */

const ROUND_MS = 1400;

const STEPS = [
  {label: 'open /login', pass: 'the form is there'},
  {label: 'fill #email', pass: 'monitor@example.com', type: true},
  {label: 'fill #password', pass: '{{login_password}}', type: true},
  {label: 'click button[type=submit]', pass: 'submitted'},
  {label: 'assert_url /dashboard', pass: 'signed in'},
];
const ROUNDS = STEPS.length;

/* `at` is the step production dies on. `ci` is the reason the same fault cannot
   reach the pipeline, which is the whole point of running the pair. */
const FAULTS = [
  {
    value: 'none', label: 'nothing broken',
    ci: 'both runs agree, which is the only time they do',
  },
  {
    value: 'secret', label: 'the OAuth secret expired',
    at: 4, why: 'url does not contain /dashboard',
    ci: 'staging has its own OAuth app, its own secret and its own expiry date',
  },
  {
    value: 'bundle', label: 'a JS file never reached the CDN',
    at: 1, why: 'selector not found: #email',
    ci: 'the runner builds the bundle itself, so there is no CDN there to lose a file',
  },
  {
    value: 'cookie', label: 'the session cookie is dropped',
    at: 4, why: 'back on /login after the submit',
    ci: 'SameSite and Secure do almost nothing on localhost, over http, on one host',
  },
  {
    value: 'botwall', label: 'bot protection challenges',
    at: 1, why: 'selector not found: #email',
    ci: 'CI traffic is allowed past the bot vendor on purpose',
  },
];
const DEFAULT_FAULT = 'secret';

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();
  let fault = FAULTS.find(f => f.value === DEFAULT_FAULT);

  const column = (side, head, cadence) => `
    <section class="mk-flow__side">
      <h4 class="mk-fig__colhead">${head}</h4>
      <ol class="mk-flow__steps">
        ${STEPS.map((s, i) => `
          <li class="mk-flow__step" data-${side}="${i}" data-state="idle">
            <span class="mk-flow__mark" aria-hidden="true"></span>
            <span class="mk-flow__label">${s.label}</span>
            <span class="mk-flow__detail"></span>
          </li>`).join('')}
      </ol>
      <p class="mk-flow__aside">${cadence}</p>
      <p class="mk-flow__badge${side === 'prod' ? ' mk-fig__verdict' : ''}" data-verdict="${side}"></p>
    </section>`;

  root.classList.add('mk-fig', 'mk-flow');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">the same test, two places</span>
      <span class="mk-fig__note">what is broken in production</span>
    </div>

    ${choices('Which fault is live', FAULTS, DEFAULT_FAULT)}

    <p class="mk-flow__aside mk-flow__why"></p>

    <div class="mk-flow__pair">
      ${column('ci', 'Your suite in CI · staging', 'runs when someone opens a pull request')}
      ${column('prod', 'The same journey · production', 'runs every five minutes, forever')}
    </div>

    <div class="mk-fig__foot">${transport('Replay both runs', `${ROUNDS} steps`)}</div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const pick = root.querySelector('.mk-fig__opts');
  const ciRows = [...root.querySelectorAll('[data-ci]')];
  const prodRows = [...root.querySelectorAll('[data-prod]')];
  const ciBadge = root.querySelector('[data-verdict="ci"]');
  const prodBadge = root.querySelector('[data-verdict="prod"]');
  const why = root.querySelector('.mk-flow__why');

  const player = {i: -1, playing: false};

  const broke = round => fault.at !== undefined && round >= fault.at;

  // CI runs the whole journey under every fault, so the replay is always the
  // full five steps even when production stopped on the second one.
  function paintRows(rows, round, dead){
    rows.forEach((el, i) => {
      const detail = el.querySelector('.mk-flow__detail');
      const stuck = dead !== undefined && i >= dead;
      const state = round < 0 || i > round || (stuck && i > dead) ? 'idle'
        : stuck ? 'down'
        : i < round ? 'ok' : 'run';
      el.dataset.state = state;
      if (state === 'idle'){ detail.textContent = ''; return; }
      if (state === 'down'){ detail.textContent = fault.why; return; }
      if (state === 'run' && STEPS[i].type && player.playing) typeOut(clocks, detail, STEPS[i].pass, ROUND_MS * 0.5);
      else detail.textContent = STEPS[i].pass;
    });
  }

  function paint(round){
    player.i = round;
    clocks.clear();
    why.textContent = fault.ci;

    paintRows(ciRows, round, undefined);
    paintRows(prodRows, round, fault.at);

    const done = round >= ROUNDS - 1;
    ciBadge.dataset.state = done ? 'ok' : '';
    ciBadge.textContent = done ? 'PASS' : round < 0 ? '' : 'running…';

    const dead = broke(round);
    prodBadge.dataset.state = dead ? 'down' : done ? 'ok' : '';
    prodBadge.textContent = dead
      ? `FAIL · step ${fault.at + 1}/${ROUNDS}`
      : done ? 'PASS' : round < 0 ? '' : 'running…';
    root.classList.toggle('is-open', dead);

    paintTransport(els, {playing: player.playing, done, round, rounds: ROUNDS, unit: 'step'});
  }

  function runFrom(round){
    paint(round);
    // Repainting here would clear the timers the frame is still running on.
    if (!player.playing || round >= ROUNDS - 1) return stop(player, els, {round, rounds: ROUNDS, unit: 'step'});
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

  // Switching fault mid-run replays from the top: the interesting frame is the
  // one where production dies, and it is rarely the frame already on screen.
  pick.addEventListener('change', e => {
    fault = FAULTS.find(f => f.value === e.target.value) || FAULTS[0];
    clocks.clear();
    player.playing = false;
    paint(-1);
    if (!calm.matches) play();
  });

  els.toggle.addEventListener('click', () => player.playing ? pause() : play());
  els.prev.addEventListener('click', () => step(player.i - 1));
  els.next.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-ci-vs-prod').forEach(mount);
