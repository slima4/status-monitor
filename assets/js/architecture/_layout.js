/* Wire routing for the architecture map: one cubic per hop, plus the point on
   it where that hop's number can sit without colliding with another badge or
   sitting on top of a node. */

export const BADGE_R = 8;
const BADGE_GAP = 2*BADGE_R + 7;                   // 2r plus air, so two numbers never touch
const BADGE_TS = [0.5,0.42,0.58,0.34,0.66,0.26,0.74,0.18,0.82];

const bezierPoint=(p0,p1,p2,p3,t)=>{
  const u=1-t, a=u*u*u, b=3*u*u*t, c=3*u*t*t, e=t*t*t;
  return {x:a*p0.x+b*p1.x+c*p2.x+e*p3.x, y:a*p0.y+b*p1.y+c*p2.y+e*p3.y};
};

const spaced=(q,taken)=>taken.every(o=>Math.hypot(o.x-q.x,o.y-q.y)>=BADGE_GAP);
const overBox=(q,b)=>q.x>b.l-BADGE_R && q.x<b.r+BADGE_R && q.y>b.t-BADGE_R && q.y<b.b+BADGE_R;
const inTheOpen=(q,obstacles)=>!obstacles.some(b=>overBox(q,b));

// A badge belongs on its own wire, so slide it along the curve until it clears
// what is already placed. Landing off a node is preferred but yields to not
// overlapping another number; only a fully crowded curve steps off it, along
// the normal.
function badgeSpot(p0,c1,c2,p3,taken,obstacles){
  const on = BADGE_TS.map(t=>bezierPoint(p0,c1,c2,p3,t));
  const best = on.find(q=>spaced(q,taken) && inTheOpen(q,obstacles)) || on.find(q=>spaced(q,taken));
  if (best) return best;

  const q=on[0], n=bezierPoint(p0,c1,c2,p3,0.52);
  const len=Math.hypot(n.x-q.x,n.y-q.y)||1, vx=-(n.y-q.y)/len, vy=(n.x-q.x)/len;
  for (let k=1;k<=4;k++) for (const dir of [1,-1]){
    const c={x:q.x+vx*BADGE_GAP*k*dir, y:q.y+vy*BADGE_GAP*k*dir};
    if (spaced(c,taken)) return c;
  }
  return q;
}

/* Steps in order → one `{d, badge}` per hop. `boxes` maps node id to its rect
   in wire-space; `obstacles` is every node rect on the map, so a number never
   comes to rest on a box. */
export function wireGeometry(steps, boxes, obstacles = []){
  const repeats = new Map(), taken = [];

  return steps.map((s,i)=>{
    const a = boxes[s.f], b = boxes[s.t];
    // The same hop can appear twice in one flow; identical endpoints would draw
    // one curve on top of another, so each repeat gets its own arc.
    const key = `${s.f}>${s.t}`, rep = repeats.get(key) || 0;
    repeats.set(key, rep + 1);
    const bow = rep === 0 ? 0 : (rep % 2 ? -1 : 1) * Math.ceil(rep/2) * 26;
    const lane = 8 + (i%3)*7 + rep*16;             // fan out overlapping wires

    let p0, c1, c2, p3;
    if (b.l - a.r > 12){                            // left → right
      const x1=a.r, y1=a.cy, x2=b.l-4, y2=b.cy, dx=Math.max(34,(x2-x1)*0.45);
      p0={x:x1,y:y1}; c1={x:x1+dx,y:y1+bow}; c2={x:x2-dx,y:y2+bow}; p3={x:x2,y:y2};
    } else if (a.l - b.r > 12){                     // right → left, route under
      const x1=a.l, y1=a.cy, x2=b.r+4, y2=b.cy, dip=Math.max(a.b,b.b)+lane+10;
      p0={x:x1,y:y1}; c1={x:x1-60,y:dip}; c2={x:x2+60,y:dip}; p3={x:x2,y:y2};
    } else {                                        // same column
      const side = a.r + lane + 14, y1=a.cy, y2=b.cy;
      p0={x:a.r,y:y1}; c1={x:side+30,y:y1}; c2={x:side+30,y:y2}; p3={x:b.r,y:y2};
    }

    const badge = badgeSpot(p0,c1,c2,p3,taken,obstacles);
    taken.push(badge);
    return {d:`M${p0.x},${p0.y} C${c1.x},${c1.y} ${c2.x},${c2.y} ${p3.x},${p3.y}`, badge};
  });
}
