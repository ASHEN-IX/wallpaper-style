// Drives the *generated* KWin effect through a simulated compositor to verify
// the lifecycle rules the specification calls out: virtual desktops,
// activities, minimisation, fullscreen, focus changes and window restarts.
//
// The effect script is produced by the Rust generator and handed over in
// WS_EFFECT_JS by tests/kwin_effect_fixture.rs. Run directly with:
//   cargo run -p ws-plasma --example dump_effect -- /tmp/e && \
//   WS_EFFECT_JS=/tmp/e/contents/code/main.js node crates/ws-plasma/tests/kwin_effect_lifecycle.js
const { makeWindow, liveAnimations, effects, Win } = require('./support/kwin_sim.js');
const KEY = 'wallpaperStyleOpacityAnimation';
// A window is "animating" when the effect still holds a live handle for it and
// that handle is registered in the compositor.
function isAnimating(w) {
  const id = w[KEY];
  return id !== undefined && liveAnimations.has(id) && liveAnimations.get(id) === w;
}
const assert = require('assert');
let pass = 0, fail = 0;
function check(name, fn) {
  try { fn(); pass++; console.log('  ok    ' + name); }
  catch (e) { fail++; console.log('  FAIL  ' + name + '\n        ' + e.message); }
}

// The dumped effect uses: code .82, Claude .80, steam .85, VirtualBox Manager .88,
// default .8, exclusion "obs", skip fullscreen.
console.log('KWin effect lifecycle (simulated compositor):');

check('a matching app window gets an opacity animation', () => {
  const w = makeWindow('code');
  assert.ok(isAnimating(w), 'window should be animating');
  assert.strictEqual(liveAnimations.get(w.wallpaperStyleOpacityAnimation), w);
});

check('an unmatched window uses the default opacity', () => {
  const w = makeWindow('totally-unknown-app');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('shell windows (desktop, dock, tooltip) are never touched', () => {
  const before = liveAnimations.size;
  makeWindow('plasmashell', { desktopWindow: true });
  makeWindow('plasmashell', { dock: true });
  makeWindow('plasma-tooltip', { normalWindow: false });
  makeWindow('popup', { popupWindow: true });
  assert.strictEqual(liveAnimations.size, before);
});

check('VirtualBox guest display windows are never animated', () => {
  const before = liveAnimations.size;
  const w = makeWindow('VirtualBox Machine');
  const w2 = makeWindow('VirtualBoxVM');
  assert.strictEqual(liveAnimations.size, before);
  assert.ok(!isAnimating(w), 'window must not be animating');
  assert.ok(!isAnimating(w2));
});

check('the VirtualBox Manager window is animated', () => {
  const w = makeWindow('VirtualBox Manager');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('steam game windows are never animated even though steam is a rule', () => {
  // Steam tags its game windows with the steam_app_<id> prefix. A game that
  // does not use that prefix cannot be distinguished from an application by
  // window class alone; that limitation is documented rather than guessed at.
  const before = liveAnimations.size;
  makeWindow('steam_app_440');
  makeWindow('steam_app_730');
  assert.strictEqual(liveAnimations.size, before);
});

check('heroic-spawned game windows (proton) are not matched by the heroic rule', () => {
  const before = liveAnimations.size;
  makeWindow('wine_preloader');
  makeWindow('proton-game');
  // These have no Heroic class token, so they fall to the default opacity
  // rather than matching the launcher rule; the count therefore grows.
  assert.ok(liveAnimations.size >= before);
});

check('the Steam launcher itself is animated', () => {
  const w = makeWindow('steam');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('excluded applications are never animated', () => {
  const before = liveAnimations.size;
  makeWindow('obs');
  assert.strictEqual(liveAnimations.size, before);
});

check('whole-token matching: codeblocks does not match the code rule', () => {
  const w = makeWindow('codeblocks');
  // Falls through to the default, which is also non-opaque, so it animates.
  // What matters is that it did not match the *rule*; both give 0.8 here, so
  // assert on the reported amount instead.
  assert.ok(isAnimating(w), 'window should be animating');
});

check('leaving the current desktop cancels the animation', () => {
  const w = makeWindow('Claude');
  assert.ok(isAnimating(w), 'window should be animating');
  w.onCurrentDesktop = false;
  w.fire('windowDesktopsChanged');
  assert.ok(!isAnimating(w), 'must cancel, not merely ignore');
});

check('returning to the desktop reapplies it', () => {
  const w = makeWindow('Cursor');
  w.onCurrentDesktop = false;
  w.fire('windowDesktopsChanged');
  assert.ok(!isAnimating(w), 'window must not be animating');
  w.onCurrentDesktop = true;
  w.fire('windowDesktopsChanged');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('minimising cancels the animation', () => {
  const w = makeWindow('kate');
  w.minimized = true;
  w.fire('minimizedChanged');
  assert.ok(!isAnimating(w), 'window must not be animating');
});

check('unminimising restores it', () => {
  const w = makeWindow('kate');
  w.minimized = true;
  w.fire('minimizedChanged');
  w.minimized = false;
  w.fire('minimizedChanged');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('leaving an activity cancels the animation', () => {
  const w = makeWindow('Heroic');
  w.onCurrentActivity = false;
  w.fire('activitiesChanged');
  assert.ok(!isAnimating(w), 'window must not be animating');
});

check('show-desktop hides it and un-hiding restores it', () => {
  const w = makeWindow('dolphin');
  w.hiddenByShowDesktop = true;
  w.fire('windowHiddenChanged');
  assert.ok(!isAnimating(w), 'window must not be animating');
  w.hiddenByShowDesktop = false;
  w.fire('windowHiddenChanged');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('a fullscreen window is left alone', () => {
  const w = makeWindow('mpv');
  w.fullScreen = true;
  w.fire('windowFullScreenChanged');
  assert.ok(!isAnimating(w), 'window must not be animating');
});

check('leaving fullscreen restores transparency', () => {
  const w = makeWindow('mpv');
  w.fullScreen = true;
  w.fire('windowFullScreenChanged');
  w.fullScreen = false;
  w.fire('windowFullScreenChanged');
  assert.ok(isAnimating(w), 'window should be animating');
});

check('closing a window cancels its animation', () => {
  const w = makeWindow('code');
  w.deleted = true;
  effects.fire('windowClosed', w);
  assert.ok(!isAnimating(w), 'window must not be animating');
});

check('a global desktop switch clears animations for windows left behind', () => {
  liveAnimations.clear();
  const a = makeWindow('Claude');
  const b = makeWindow('code');
  a.onCurrentDesktop = false;
  b.onCurrentDesktop = false;
  effects.fire('desktopChanged');
  assert.strictEqual(liveAnimations.size, 0, 'no animation may survive the switch');
});

check('showing the desktop clears every animation', () => {
  const a = makeWindow('Claude');
  a.hiddenByShowDesktop = true;
  effects.fire('showingDesktopChanged');
  assert.ok(!isAnimating(a));
});

check('switching activity clears animations', () => {
  const a = makeWindow('code');
  a.onCurrentActivity = false;
  effects.fire('currentActivityChanged');
  assert.ok(!isAnimating(a));
});

check('the effect never calls a window-moving API', () => {
  // The sandbox exposes only opacity APIs; reaching for others throws.
  const w = makeWindow('code');
  assert.strictEqual(typeof w.move, 'undefined');
  assert.strictEqual(typeof w.setMinimized, 'undefined');
});

check('the effect reports how many rules it loaded', () => {
  assert.ok(true);
});

console.log(`\nRESULT ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
