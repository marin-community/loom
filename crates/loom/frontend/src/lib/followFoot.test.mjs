import test from 'node:test';
import assert from 'node:assert/strict';
import { effectScope, nextTick, ref } from 'vue';
import { useFollowFoot } from './followFoot.ts';

// A minimal scroll container. A real browser clamps `scrollTop` to the content
// and reports no client rects while the element is `display:none`; some
// browsers (notably Safari) also drop its offset, which is the case a docked
// artifact creates when it hides the conversation.
class FakeScroller {
  constructor() {
    this.scrollHeight = 2000;
    this.clientHeight = 100;
    this.style = {};
    this._top = 0;
    this.layout = true;
  }

  get maxTop() {
    return Math.max(0, this.scrollHeight - this.clientHeight);
  }

  get scrollTop() {
    return this._top;
  }

  set scrollTop(value) {
    this._top = Math.max(0, Math.min(Number(value) || 0, this.maxTop));
  }

  getClientRects() {
    return this.layout ? [{ top: 0 }] : [];
  }

  hide() {
    this.layout = false;
    this.clientHeight = 0;
    this.scrollHeight = 0;
    this._top = 0;
  }

  show({ restoreTop = 0 } = {}) {
    this.layout = true;
    this.clientHeight = 100;
    this.scrollHeight = 2000;
    this._top = restoreTop;
  }
}

async function harness({ withActive = false } = {}) {
  const previousRO = globalThis.ResizeObserver;
  let onResize = null;
  globalThis.ResizeObserver = class {
    constructor(callback) {
      onResize = callback;
    }
    observe() {}
    disconnect() {}
  };
  const scroller = new FakeScroller();
  const scrollEl = ref(null);
  const bodyEl = ref(null);
  const active = withActive ? ref(true) : undefined;
  const scope = effectScope();
  const foot = scope.run(() => useFollowFoot(scrollEl, bodyEl, active));
  // Mirror mount: the refs start null and receive their elements after render.
  scrollEl.value = scroller;
  bodyEl.value = {};
  await nextTick(); // attach the body watcher / observer
  return {
    scroller,
    foot,
    active,
    resize: () => onResize?.(),
    done: () => {
      scope.stop();
      globalThis.ResizeObserver = previousRO;
    },
  };
}

test('restores a released reading position when the pane is shown again', async () => {
  const h = await harness();
  try {
    h.resize(); // initial layout: a fresh chat follows to the foot
    assert.equal(h.scroller.scrollTop, h.scroller.maxTop);

    h.scroller.scrollTop = 400;
    h.foot.trackPin();
    assert.equal(h.foot.pinned.value, false);

    h.scroller.hide();
    h.resize(); // the docked artifact hides the conversation
    assert.equal(h.foot.pinned.value, false);

    // A browser that drops the hidden offset shows the pane at the top.
    h.scroller.show();
    h.resize();
    assert.equal(h.scroller.scrollTop, 400);
    assert.equal(h.foot.pinned.value, false);
  } finally {
    h.done();
  }
});

test('a hidden reset scroll event does not release the saved place', async () => {
  const h = await harness();
  try {
    h.resize();
    h.scroller.scrollTop = 400;
    h.foot.trackPin();

    h.scroller.hide();
    h.resize();
    h.foot.trackPin(); // queued scroll event from the hidden/reset state (top 0)

    h.scroller.show();
    h.resize();
    assert.equal(h.scroller.scrollTop, 400);
  } finally {
    h.done();
  }
});

test('a pinned reader returns to the newest exchange, not the old offset', async () => {
  const h = await harness();
  try {
    h.resize(); // pinned at the foot
    h.scroller.hide();
    h.resize();

    h.scroller.show();
    h.resize();
    assert.equal(h.foot.pinned.value, true);
    assert.equal(h.scroller.scrollTop, h.scroller.maxTop);
  } finally {
    h.done();
  }
});

test('a pane-signal show restores without a layout notification', async () => {
  const h = await harness({ withActive: true });
  try {
    h.resize();
    h.scroller.scrollTop = 400;
    h.foot.trackPin();

    h.scroller.hide();
    h.active.value = false;
    await nextTick();

    // The pane comes back, but the engine dropped the hidden offset and never
    // delivered a ResizeObserver callback for the show.
    h.scroller.show();
    h.active.value = true;
    await nextTick();
    await nextTick();
    assert.equal(h.scroller.scrollTop, 400);
  } finally {
    h.done();
  }
});

test('a hidden pane signal ignores reset scroll events while laid out', async () => {
  const h = await harness({ withActive: true });
  try {
    h.resize();
    h.scroller.scrollTop = 400;
    h.foot.trackPin();

    // The artifact is stable, but a transient layout (or a browser reset) puts
    // the scroller back at the top while the pane signal still says hidden.
    h.active.value = false;
    await nextTick();
    h.scroller.scrollTop = 0;
    h.foot.trackPin();

    h.scroller.show();
    h.active.value = true;
    await nextTick();
    await nextTick();
    assert.equal(h.scroller.scrollTop, 400);
  } finally {
    h.done();
  }
});

test('a layout hide/show restores while the pane signal stays active', async () => {
  const h = await harness({ withActive: true });
  try {
    h.resize();
    h.scroller.scrollTop = 400;
    h.foot.trackPin();

    // Keep-alive deactivation parks the page: the tab still says active, but the
    // pane leaves the layout until the user returns.
    h.scroller.hide();
    h.resize();
    h.scroller.show();
    h.resize();
    assert.equal(h.scroller.scrollTop, 400);
  } finally {
    h.done();
  }
});
