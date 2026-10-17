// Follow-the-foot scroll for a chat surface (shared by AcpConversation and
// TerminalConversation). The transcript opens at its newest exchange and stays
// *pinned* there while content grows; scrolling up releases the pin, scrolling
// back to the foot re-arms it. Growth lands asynchronously — markdown parses
// off-tick, images load, the pane un-hides from a v-show tab — so a
// ResizeObserver on the transcript body does the following; a one-shot scroll
// after nextTick would race the paint and strand the view mid-history.

import { ref, watch, onUnmounted, nextTick, type Ref } from 'vue';

export function useFollowFoot(scrollEl: Ref<HTMLElement | null>, bodyEl: Ref<HTMLElement | null>) {
  // Starts pinned, so a fresh chat scrolls to the foot as soon as it has height.
  const pinned = ref(true);

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
    el.scrollTop = el.scrollHeight;
    autoScrollTarget = el.scrollTop; // the clamped position actually applied
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
    if (autoScrollTarget != null) {
      if (Math.abs(el.scrollTop - autoScrollTarget) < 1) return; // our echo — the pin stands
      autoScrollTarget = null; // the reader has scrolled since — fall through and derive
    }
    pinned.value = nearBottom();
  }

  // Browser scroll anchoring must be off on the container: when a late-painting
  // block lands above the viewport, anchoring silently moves scrollTop to keep
  // the view stable — a scroll we didn't make, at a position that no longer
  // matches our target, which trackPin would misread as the reader scrolling
  // away and wrongly release the pin. This surface has exactly two legitimate
  // scrollers: this composable and the reader.
  watch(scrollEl, (el) => {
    if (el) el.style.overflowAnchor = 'none';
  });

  let bodyRO: ResizeObserver | null = null;
  watch(bodyEl, (el) => {
    bodyRO?.disconnect();
    if (!el) return;
    bodyRO ??= new ResizeObserver(() => {
      if (pinned.value) scrollToBottom();
    });
    bodyRO.observe(el);
  });
  onUnmounted(() => {
    bodyRO?.disconnect();
    bodyRO = null;
  });

  return { pinned, scrollToBottom, scrollToEnd, autoFollow, trackPin };
}
