// Minimal simulation of the KWin scripting host, enough to exercise the
// generated effect's lifecycle logic. Validates behaviour that cannot be
// observed from outside a live compositor.
const fs = require('fs');
const script = fs.readFileSync(process.env.WS_EFFECT_JS, 'utf8');

const SIGNALS = ['windowDesktopsChanged','windowHiddenChanged','windowFullScreenChanged',
                 'activitiesChanged','windowShown','windowHidden','minimizedChanged'];

let nextId = 1;
const liveAnimations = new Map();
let lastWindow = null;

class Connector {
  constructor(win, name) { this.win = win; this.name = name; }
  connect(fn) { (this.win._handlers[this.name] ??= []).push(fn); }
}

class Win {
  constructor(cls, opts) {
    Object.assign(this, {
      windowClass: cls, normalWindow: true, dialog: false, desktopWindow: false,
      dock: false, popupWindow: false, managed: true, deleted: false,
      minimized: false, hiddenByShowDesktop: false, visible: true,
      onCurrentDesktop: true, onCurrentActivity: true, fullScreen: false,
      repaints: 0, _handlers: {},
    }, opts || {});
    for (const name of SIGNALS) { this[name] = new Connector(this, name); }
    lastWindow = this;
  }
  // Deliberately NOT provided: KWin's window object has no addRepaintFull.
  // The first version of this simulation did provide it, which certified a
  // script that failed to load on a real compositor. Repaints are requested on
  // the effects handler instead, exactly as the real API requires.
  fire(name) { (this._handlers[name] || []).forEach(fn => fn()); }
}

const windows = [];
const effects = {
  stackingOrder: windows,
  _cb: {},
};
for (const ev of ['windowAdded','windowClosed','windowDeleted','desktopChanged','desktopChangingCancelled',
                  'currentActivityChanged','showingDesktopChanged','windowActivated']) {
  effects[ev] = { connect(fn) { (effects._cb[ev] ??= []).push(fn); } };
}
effects.fire = (ev, ...args) => (effects._cb[ev] || []).forEach(fn => fn(...args));
// The real effects handler provides this; the generated script calls it.
effects.addRepaintFull = function () { effects.repaints++; };
effects.repaints = 0;

const Effect = { Opacity: 1 };
function set(opts) {
  const id = nextId++;
  liveAnimations.set(id, opts.window);
  opts.window._anim = id;
  return id;
}
function cancel(id) { if (id !== undefined) liveAnimations.delete(id); }

const sink = [];
new Function('effects','Effect','set','cancel','print', script)(
  effects, Effect, set, cancel, m => sink.push(m));

function makeWindow(cls, opts) {
  const w = new Win(cls, opts);
  windows.push(w);
  effects.fire('windowAdded', w);
  return w;
}

module.exports = { makeWindow, windows, liveAnimations, effects, sink, Win };
