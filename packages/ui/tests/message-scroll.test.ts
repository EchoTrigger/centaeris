import { describe, it, expect, vi } from "vitest";
import { createMessageScroll } from "../src/components/chat/messageScroll";

const motion = vi.hoisted(() => ({ current: null as null | { progress(value: number): void; stop(): void } }));
vi.mock("motion", () => ({ animate: (_from: number, _to: number, options: { onUpdate(value: number): void; onComplete(): void }) => {
  let active = true;
  const playback = {
    progress(value: number) {
      if (!active) return;
      options.onUpdate(value);
      if (value === 1) { active = false; options.onComplete(); }
    },
    stop() { active = false; },
  };
  motion.current = playback;
  return playback;
} }));

describe("message anchoring", () => {
  function fixture(reducedMotion = false) {
    const geometry = { height: 800, contentEnd: 1300, anchorTop: 1200, scrollTop: 500 };
    let padding = 0;
    let following = true;
    const scroll = createMessageScroll({
      measure: () => geometry,
      setPadding: (value) => { padding = value; },
      scrollTo: (value) => { geometry.scrollTop = value; },
      reducedMotion: () => reducedMotion,
      onFollowingChange: value => { following = value; },
    });
    return { geometry, scroll, following: () => following, padding: () => padding, progress: (value: number) => motion.current?.progress(value) };
  }

  it("eases a sent message to one quarter and consumes space as content grows", () => {
    const f = fixture();
    f.scroll.anchor();
    f.progress(0); f.progress(0.5);
    expect(f.geometry.scrollTop).toBeGreaterThan(500);
    expect(f.geometry.scrollTop).toBeLessThan(1000);
    f.progress(1);
    expect(f.geometry.scrollTop).toBe(1000);
    expect(f.padding()).toBe(500);
    f.geometry.contentEnd = 1600; f.scroll.update();
    expect(f.padding()).toBe(200);
    expect(f.geometry.scrollTop).toBe(1000);
    f.geometry.contentEnd = 2000; f.scroll.update();
    expect(f.padding()).toBe(0);
    expect(f.geometry.scrollTop).toBe(1200);
  });


  it("keeps sent-turn following and reserved reply space through history remeasurement", () => {
    const f = fixture(true);
    f.scroll.anchor();
    f.geometry.anchorTop -= 40;
    f.geometry.contentEnd -= 40;
    f.geometry.scrollTop -= 40;
    f.scroll.userScroll();
    expect(f.following()).toBe(true);
    expect(f.padding()).toBe(500);
    f.geometry.contentEnd += 200;
    f.scroll.update();
    expect(f.padding()).toBe(300);
    f.geometry.contentEnd += 600;
    f.scroll.update();
    expect(f.following()).toBe(true);
    expect(f.padding()).toBe(0);
    expect(f.geometry.scrollTop).toBe(f.geometry.contentEnd - f.geometry.height);
  });

  it("retains the sent-turn Motion animation when virtual rows compensate its offset", () => {
    const f = fixture();
    f.scroll.anchor();
    f.progress(0.25);
    f.geometry.anchorTop += 40;
    f.geometry.contentEnd += 40;
    f.geometry.scrollTop += 40;
    f.scroll.userScroll();
    f.progress(1);
    expect(f.following()).toBe(true);
    expect(f.geometry.scrollTop).toBe(1040);
    expect(f.padding()).toBe(500);
  });

  it("reclaims blank space while reading upward without moving the reading position", () => {
    const f = fixture(true);
    f.scroll.anchor(); f.scroll.pause();
    f.geometry.scrollTop = 700; f.scroll.userScroll();
    expect(f.padding()).toBe(200);
    expect(f.geometry.scrollTop).toBe(700);
    f.geometry.scrollTop = 400; f.scroll.userScroll();
    expect(f.padding()).toBe(0);
    f.geometry.contentEnd = 1800; f.scroll.update();
    expect(f.geometry.scrollTop).toBe(400);
  });

  it("cancels animation on user intent and resets space on session change", () => {
    const f = fixture();
    f.scroll.anchor(); f.progress(0); f.progress(0.25);
    f.scroll.pause();
    const position = f.geometry.scrollTop;
    f.progress(1);
    expect(f.geometry.scrollTop).toBe(position);
    f.scroll.reset();
    expect(f.padding()).toBe(0);
  });
  it("accounts for short content and adjusts the anchor after admission or resize", () => {
    const f = fixture(true);
    f.geometry.contentEnd = 500;
    f.geometry.anchorTop = 400;
    f.scroll.anchor();
    expect(f.geometry.scrollTop).toBe(200);
    expect(f.padding()).toBe(500);
    f.geometry.anchorTop = 420;
    f.geometry.contentEnd = 520;
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(220);
    f.geometry.height = 400;
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(320);
    expect(f.padding()).toBe(200);
  });

  it("retargets a moving message during the anchoring interaction", () => {
    const f = fixture();
    f.scroll.anchor(); f.progress(0); f.progress(0.25);
    f.geometry.anchorTop = 1400;
    f.geometry.contentEnd = 1500;
    f.progress(0.5); f.progress(1);
    expect(f.geometry.scrollTop).toBe(1200);
    expect(f.padding()).toBe(500);
  });

  it("explicit reading intent cancels anchoring and later content cannot pull the reader down", () => {
    const f = fixture();
    f.scroll.anchor(); f.progress(0); f.progress(0.25);
    f.scroll.pause();
    f.geometry.scrollTop = 300;
    f.scroll.userScroll();
    f.progress(1);
    f.geometry.contentEnd = 1800; f.scroll.update();
    expect(f.geometry.scrollTop).toBe(300);
  });


  it("keeps following when a programmatic scroll event arrives after content growth", () => {
    const f = fixture();
    f.geometry.scrollTop = 0;
    f.scroll.reset();
    f.scroll.update();
    f.geometry.contentEnd += 500;
    f.scroll.userScroll();
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(f.geometry.contentEnd - f.geometry.height);
  });

  it("keeps following when virtual row compensation advances the scroll offset", () => {
    const f = fixture();
    f.scroll.reset();
    f.scroll.update();
    f.geometry.contentEnd += 500;
    f.geometry.scrollTop += 100;
    f.scroll.userScroll();
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(f.geometry.contentEnd - f.geometry.height);
  });


  it("starts a new conversation at latest even if a scroll event precedes its first positioning frame", () => {
    const f = fixture();
    f.scroll.update();
    f.scroll.reset();
    f.geometry.scrollTop = 0;
    f.geometry.contentEnd += 500;
    f.scroll.userScroll();
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(f.geometry.contentEnd - f.geometry.height);
  });

  it("keeps following when row remeasurement moves a programmatic offset backward", () => {
    const f = fixture();
    f.scroll.update();
    f.geometry.contentEnd += 300;
    f.geometry.scrollTop -= 100;
    f.scroll.userScroll();
    f.geometry.contentEnd += 500;
    f.scroll.update();
    expect(f.geometry.scrollTop).toBe(f.geometry.contentEnd - f.geometry.height);
  });

});
