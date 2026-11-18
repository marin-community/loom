// Follow-the-foot scroll for a chat surface (shared by AcpConversation and
// TerminalConversation). The transcript opens at its newest exchange and stays
// *pinned* there while content grows; scrolling up releases the pin, scrolling
// back to the foot re-arms it. Growth lands asynchronously — markdown parses
// off-tick, images load, the pane un-hides from a v-show tab — so a
// ResizeObserver on the transcript body does the following; a one-shot scroll
// after nextTick would race the paint and strand the view mid-history.

import { ref, watch, onScopeDispose, nextTick, type Ref } from 'vue';

export function useFollowFoot(
  scrollEl: Ref<HTMLElement | null>,
  bodyEl: Ref<HTMLElement | null>,
  /** Whether the hosting pane is on screen. When supplied, the pane state owns
   *  the hidden→shown edge instead of inferring it from layout notifications. */
  active?: Ref<boolean>,
) {
  // Starts pinned, so a fresh chat scrolls to the foot as soon as it has height.
  const pinned = ref(true);

  // The reader's place. A chat pane is hidden with `display:none` while an
  // artifact is docked over it (and on the route to/from that artifact). Some
  // browsers drop a hidden scroll container's offset, and history restoration
  // cannot put it back when the element was not laid out as the navigation
  // landed — so remember the position while visible and restore it on return.
  let lastTop = 0;
  let visible = active ? active.value : true;

  function laidOut(el: HTMLElement): boolean {
    return el.getClientRects().length > 0;
  }

  // Whether the stream is scrolled to (near) its foot. A missing scroll root
  // counts as "at the bottom" (nothing scrolled yet).
  function nearBottom(): boolean {
    const el = scrollEl.value;
    if (!el) return true;
    return el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  }
  // Scroll events dispatch asynchronously, so a stale event from one of our own
  // scrollToBottom calls can land *after* the content has grown again — and a
  // handler that re-derived the pin from that snapshot would see "far from the
  // foot" and wrongly release it, stranding the view mid-history. So trackPin
  // swallows our own echoes, recognised by position: while the view still sits
  // exactly where the last programmatic scroll put it, the pin stands; the pin
  // only ever re-derives from a scroll that actually moved the view.
  let autoScrollTarget: number | null = null;
  function scrollToBottom() {
    const el = scrollEl.value;
    if (!el) return;
    // Hidden (display:none): the offset reads zero and writing it would discard
    // the reader's place. The show edge follows once it is laid out.
    if (!laidOut(el)) return;
    el.scrollTop = el.scrollHeight;
    autoScrollTarget = el.scrollTop; // the clamped position actually applied
    lastTop = el.scrollTop;
  }
  /** Put the reader back where they left unless they were following the foot. */
  function restorePlace() {
    const el = scrollEl.value;
    if (!el) return;
    if (pinned.value) {
      scrollToBottom();
      return;
    }
    if (!laidOut(el)) return;
    el.scrollTop = lastTop;
  }
  /** Handle a hidden pane becoming visible: follow the foot or restore the place. */
  function returnToView() {
    const el = scrollEl.value;
    if (!el || !laidOut(el)) return;
    const returning = !visible;
    visible = true;
    if (returning) {
      restorePlace();
    } else if (pinned.value) {
      scrollToBottom();
    }
  }
  /** Post-mutation follow: scroll after the DOM settles, only while pinned. */
  function autoFollow() {
    if (pinned.value) nextTick(scrollToBottom);
  }
  /** Re-arm the pin and jump to the foot — the reader asked for the end
   *  explicitly, so following resumes from here. */
  function scrollToEnd() {
    pinned.value = true;
    scrollToBottom();
  }
  /** The scroll-handler half: re-derive the pin from the reader's position. */
  function trackPin() {
    const el = scrollEl.value;
    if (!el) return;
    // A hide/reset scroll event carries a meaningless zero and must not release
    // the reader's saved place.
    if (!visible || !laidOut(el)) return;
    if (autoScrollTarget != null) {
      if (Math.abs(el.scrollTop - autoScrollTarget) < 1) return; // our echo — the pin stands
      autoScrollTarget = null; // the reader has scrolled since — fall through and derive
    }
    pinned.value = nearBottom();
    lastTop = el.scrollTop;
  }

  if (active) {
    // Own the edge from the pane state. The hide half runs before the DOM flips
    // so a reset scroll event is ignored; the show half runs after the pane has
    // been laid out again.
    watch(
      active,
      (now) => {
        if (!now) visible = false;
      },
      { flush: 'sync' },
    );
    watch(
      active,
      (now) => {
        if (now) nextTick(returnToView);
      },
      { flush: 'post' },
    );
  }

  // Browser scroll anchoring must be off on the container: when a late-painting
  // block lands above the viewport, anchoring silently moves scrollTop to keep
  // the view stable — a scroll we didn't make, at a position that no longer
  // matches our target, which trackPin would misread as the reader scrolling
  // away and wrongly release the pin. This surface has exactly two legitimate
  // scrollers: this composable and the reader.
  watch(scrollEl, (el) => {
    if (!el) return;
    el.style.overflowAnchor = 'none';
    // A replaced scroller (a fresh transcript load) starts in whatever layout
    // state the pane is in; only a later show edge should restore the place.
    if (!active) visible = laidOut(el);
  });

  let bodyRO: ResizeObserver | null = null;
  watch(bodyEl, (el) => {
    bodyRO?.disconnect();
    if (!el) return;
    bodyRO ??= new ResizeObserver(() => {
      const scroller = scrollEl.value;
      if (!scroller) return;
      if (!laidOut(scroller)) {
        // Any unrendered layout (the work-area tab, a docked artifact, or the
        // kept-alive page being parked off-screen) means the next render is a
        // return whose place must be restored.
        visible = false;
        return;
      }
      if (active) {
        // The pane state also owns hide/show, covering an engine that coalesces
        // or skips the layout edge; layout notifications handle the rest.
        if (!visible) {
          if (active.value) returnToView();
        } else if (pinned.value) {
          scrollToBottom();
        }
        return;
      }
      returnToView();
    });
    bodyRO.observe(el);
  });
  onScopeDispose(() => {
    bodyRO?.disconnect();
    bodyRO = null;
  });

  return { pinned, scrollToBottom, scrollToEnd, autoFollow, trackPin };
}
