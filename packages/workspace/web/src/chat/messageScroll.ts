type Geometry = { height: number; contentEnd: number; anchorTop: number; scrollTop: number };
type Port = {
  measure(): Geometry | null;
  setPadding(value: number): void;
  scrollTo(value: number): void;
  requestFrame(callback: (time: number) => void): number;
  cancelFrame(id: number): void;
  reducedMotion(): boolean;
  onFollowingChange(value: boolean): void;
};

// Both clients use this controller. Geometry comes from actual visible content,
// including measured virtual rows, never from the height of hidden process text.
export function createMessageScroll(port: Port) {
  let following = true;
  let anchored = false;
  let padding = 0;
  let frame: number | null = null;
  let target = 0;
  let lastScrollTop = 0;
  const scrollTo = (top: number) => { lastScrollTop = top; port.scrollTo(top); };
  const setFollowing = (value: boolean) => {
    following = value;
    port.onFollowingChange(value);
  };
  const setPadding = (value: number) => {
    if (value === padding) return;
    padding = value;
    port.setPadding(value);
  };
  const cancel = () => {
    if (frame !== null) port.cancelFrame(frame);
    frame = null;
  };
  const update = () => {
    const g = port.measure();
    if (!g) return;
    if (!following) {
      // Reclaim only space below the current viewport. Removing it all at once
      // would clamp scrollTop and make the user's reading position jump.
      setPadding(Math.min(padding, Math.max(0, g.scrollTop + g.height - g.contentEnd)));
      return;
    }
    const naturalEnd = Math.max(0, g.contentEnd - g.height);
    target = anchored ? Math.max(naturalEnd, g.anchorTop - g.height / 4, 0) : naturalEnd;
    setPadding(anchored ? Math.max(0, target + g.height - g.contentEnd) : 0);
    if (anchored && padding === 0) anchored = false;
    if (frame === null) scrollTo(target);
  };
  return {
    update,
    anchor() {
      cancel();
      const g = port.measure();
      if (!g) return;
      anchored = true;
      setFollowing(true);
      if (port.reducedMotion()) { update(); return; }
      const start = g.scrollTop;
      lastScrollTop = start;
      let began: number | null = null;
      const tick = (time: number) => {
        began ??= time;
        update();
        const progress = Math.min(1, (time - began) / 320);
        scrollTo(start + (target - start) * (1 - (1 - progress) ** 3));
        frame = progress < 1 ? port.requestFrame(tick) : null;
      };
      frame = port.requestFrame(tick);
      update();
    },
    pause() { cancel(); anchored = false; setFollowing(false); update(); },
    userScroll() {
      const g = port.measure();
      if (!g) return;
      if (frame !== null || anchored) {
        if (Math.abs(g.scrollTop - lastScrollTop) <= 2) return;
        cancel(); anchored = false; setFollowing(false); update();
        return;
      }
      // A temporary tail must never count as the real end when resuming follow.
      const atEnd = Math.abs(g.contentEnd - g.height - g.scrollTop) <= 2;
      if (atEnd && padding === 0) setFollowing(true);
      else if (g.scrollTop < g.contentEnd - g.height - 2) setFollowing(false);
      update();
    },
    jump() { cancel(); anchored = false; setFollowing(true); update(); },
    reset() { cancel(); anchored = false; setPadding(0); setFollowing(true); },
    dispose: cancel,
  };
}
