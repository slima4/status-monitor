import { autoplay, mix } from './_figure.js';

/* A week of checks, and the far smaller thing the status page is allowed to say.

   Three region lanes carry every check bucket. The band underneath carries only
   what two regions agreed on, which is the rule the incident writer uses. The
   gap between the red in the lanes and the red in the band is the whole point:
   a status page that publishes the lanes cries wolf, and one that publishes a
   typed-in number says nothing at all. */

const SWEEP_MS = 12000;
const LOG_LINES = 4;
const DAYS = 7;
const HOURS = DAYS * 24;
const WEEK_MIN = DAYS * 24 * 60;
const INTERVAL_S = 20;
const QUORUM = 2;

const REGIONS = ['eu-helsinki', 'apac-sg', 'us-east'];

/* Minute-precise so the published number is derived rather than asserted, then
   bucketed to hours only for drawing. A window that only one region sees is the
   interesting case: it is real, it is red, and it is not an outage. */
const OUTAGES = [
  {at: 1240, min: 12, regions: [0], why: 'transit flap into Helsinki'},
  {at: 2880, min: 7, regions: [1], why: 'packet loss on one upstream'},
  {at: 4010, min: 9, regions: [0], why: 'resolver timeout'},
  {at: 5220, min: 6, regions: [2], why: 'peering wobble'},
  {at: 6602, min: 11, regions: [0, 1, 2], why: 'origin returning 502'},
  {at: 7400, min: 5, regions: [1], why: 'transient TLS reset'},
  {at: 8815, min: 4, regions: [0, 2], why: 'certificate reload'},
  {at: 9500, min: 8, regions: [1], why: 'route change upstream'},
];

const hourOf = min => Math.floor(min / 60);

/** Lane buckets, the confirmed band, and every number the card prints. */
function build(){
  const lanes = REGIONS.map(() => new Array(HOURS).fill(0));
  const band = new Array(HOURS).fill(0);

  for (let r = 0; r < REGIONS.length; r++){
    for (let h = 0; h < HOURS; h++){
      if (mix(r * 7919 + h) % 12 === 0) lanes[r][h] = 1;
    }
  }

  for (const o of OUTAGES) for (const r of o.regions) lanes[r][hourOf(o.at)] = 2;

  // The same rule the incident writer uses, so the published number and the
  // alert can never tell different stories.
  let downMin = 0;
  let soloMin = 0;
  for (const o of OUTAGES){
    if (o.regions.length >= QUORUM){
      downMin += o.min;
      band[hourOf(o.at)] = 2;
    } else {
      soloMin += o.min;
    }
  }

  return {
    lanes, band, downMin, soloMin,
    checks: WEEK_MIN * 60 / INTERVAL_S * REGIONS.length,
  };
}

const D = build();

const CLOCK = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const stamp = h => `${CLOCK[Math.floor(h / 24)]} ${String(h % 24).padStart(2, '0')}:00`;
const STATE = ['every check passed', 'slower than usual, still passing', 'checks failing'];

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');

  const lane = (r) => `
    <div class="mk-week-lane">
      <span class="mk-week-lane__name">${REGIONS[r]}</span>
      <svg viewBox="0 0 ${HOURS} 10" class="mk-week-lane__plot" preserveAspectRatio="none" role="img"
           aria-label="${REGIONS[r]}: one bar per hour across seven days">
        ${D.lanes[r].map((s, h) => `<rect x="${h}" y="${s === 2 ? 0 : 2}" width="0.82" height="${s === 2 ? 10 : 6}" data-h="${h}" data-s="${s}"><title>${stamp(h)} · ${REGIONS[r]} · ${STATE[s]}</title></rect>`).join('')}
      </svg>
      <span class="mk-week-lane__mark">ok</span>
    </div>`;

  root.classList.add('mk-fig', 'mk-week');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">modelled week</span>
      <span class="mk-fig__note">what the checks saw, and what the page may say</span>
    </div>

    <div class="mk-week__readout">
      <p class="mk-week__big"><span data-stat="uptime">100.000%</span><span class="mk-week__unit"> measured uptime</span></p>
      <p class="mk-week__clock" data-stat="clock">Mon 00:00</p>
    </div>

    <div class="mk-week__plot">
      <div class="mk-week__axis" aria-hidden="true">${CLOCK.map(d => `<span>${d}</span>`).join('')}</div>
      ${REGIONS.map((_, r) => lane(r)).join('')}

      <div class="mk-week-lane mk-week-lane--band">
        <span class="mk-week-lane__name">status page</span>
        <svg viewBox="0 0 ${HOURS} 10" class="mk-week-lane__plot" preserveAspectRatio="none" role="img"
             aria-label="Confirmed incidents published to the status page">
          ${D.band.map((s, h) => `<rect x="${h}" y="${s ? 0 : 4}" width="0.82" height="${s ? 10 : 2}" data-h="${h}" data-s="${s}"><title>${stamp(h)} · ${s ? 'confirmed incident' : 'operational'}</title></rect>`).join('')}
        </svg>
        <span class="mk-week-lane__mark">up</span>
      </div>
    </div>

    <p class="mk-fig__legend" aria-hidden="true">
      <span class="mk-fig__key mk-fig__key--ok"></span>passing
      <span class="mk-fig__key mk-fig__key--setup"></span>slow
      <span class="mk-fig__key mk-fig__key--down"></span>failing
      <span class="mk-fig__key mk-fig__key--wait"></span>confirmed, and published
    </p>

    <dl class="mk-week__stats">
      <div><dt>checks</dt><dd data-stat="checks">0</dd></div>
      <div><dt>one region only</dt><dd data-stat="solo">0</dd></div>
      <div><dt>agreed by ${QUORUM}</dt><dd data-stat="down">0</dd></div>
      <div><dt>incidents published</dt><dd data-stat="inc">0</dd></div>
    </dl>

    <ol class="mk-week__log" aria-hidden="true"></ol>
    <p class="mk-fig__verdict mk-week__sum" role="status" aria-live="polite"></p>

    <div class="mk-fig__foot mk-fig__foot--split">
      <button type="button" class="mk-fig__go" data-act="replay">replay the week</button>
      <span class="mk-fig__prov">7 days · ${REGIONS.length} regions · one check every ${INTERVAL_S}s · modelled, not measured</span>
    </div>`;

  const stat = n => root.querySelector(`[data-stat="${n}"]`);
  const bars = [...root.querySelectorAll('.mk-week-lane__plot rect')];
  const marks = [...root.querySelectorAll('.mk-week-lane__mark')];
  const log = root.querySelector('.mk-week__log');
  const summary = root.querySelector('.mk-week__sum');
  const btn = root.querySelector('[data-act="replay"]');

  let raf = 0;
  let drawn = null;

  function paint(hour, done){
    // 168 hours across a 12s sweep is ~4 rAF frames per bucket, and a repaint
    // rewrites 672 rects and the log. Skip the frames that would draw the same
    // thing.
    const key = `${hour}/${done}`;
    if (key === drawn) return;
    drawn = key;

    for (const b of bars){
      const shown = +b.dataset.h <= hour;
      b.setAttribute('fill-opacity', shown ? '1' : '0.12');
    }

    // Counters only ever count what the playhead has already passed, so the
    // published number is seen being earned rather than announced.
    let solo = 0, down = 0, inc = 0;
    for (const o of OUTAGES){
      if (hourOf(o.at) > hour) continue;
      if (o.regions.length >= QUORUM){ down += o.min; inc++; } else solo += o.min;
    }
    const elapsed = done ? WEEK_MIN : Math.min(WEEK_MIN, (hour + 1) * 60);

    stat('uptime').textContent = `${((elapsed - down) / elapsed * 100).toFixed(3)}%`;
    stat('clock').textContent = done ? 'Sun 23:00' : stamp(Math.max(0, hour));
    stat('checks').textContent = Math.round(D.checks * (elapsed / WEEK_MIN)).toLocaleString();
    stat('solo').textContent = `${solo} min`;
    stat('down').textContent = `${down} min`;
    stat('inc').textContent = inc;

    marks.forEach((m, i) => {
      const band = i === REGIONS.length;
      const s = band ? D.band[Math.max(0, hour)] : D.lanes[i][Math.max(0, hour)];
      m.dataset.state = s === 2 ? 'down' : s === 1 ? 'slow' : 'ok';
      m.textContent = band ? (s ? 'incident' : 'up') : s === 2 ? 'failing' : s === 1 ? 'slow' : 'ok';
    });

    // Rebuilt from the playhead rather than appended on the way past, so a
    // replay or a jump to the end lands on the same log either way.
    const seen = OUTAGES.filter(o => hourOf(o.at) <= hour);
    const lines = seen.slice(-LOG_LINES).map(o => {
      const published = o.regions.length >= QUORUM;
      const tail = published
        ? `${o.regions.length} regions agree · ${o.min} min published`
        : `${REGIONS[o.regions[0]]} alone · not published`;
      return `<li data-kind="${published ? 'published' : 'solo'}">${stamp(hourOf(o.at))} · ${o.why} · ${tail}</li>`;
    });

    // The closing line lands in the log rather than in a block of its own, so
    // nothing has to hold empty space open for it while the week replays.
    const closing = `${D.soloMin} minutes one region saw alone · ${D.downMin} that ${QUORUM} agreed on · only the second is publishable`;
    if (done) lines.push(`<li data-kind="summary">${closing}</li>`);
    log.innerHTML = lines.join('');
    summary.textContent = done ? closing : '';

  }

  function play(){
    cancelAnimationFrame(raf);
    // play() clears the log by hand, so the next paint must not be skipped.
    drawn = null;
    if (calm.matches){ paint(HOURS - 1, true); return; }
    summary.textContent = '';
    log.innerHTML = '';
    const start = performance.now();
    const step = now => {
      const p = Math.min(1, (now - start) / SWEEP_MS);
      paint(Math.floor(p * (HOURS - 1)), p >= 1);
      if (p < 1) raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
  }

  btn.addEventListener('click', play);
  paint(HOURS - 1, true);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-measured-week').forEach(mount);
