import { describe, it, expect } from "vitest";
import { createMessageScroll } from "../src/components/chat/messageScroll";

describe("message anchoring", () => {
  function fixture(reducedMotion = false) {
    const geometry = { height: 800, contentEnd: 1300, anchorTop: 1200, scrollTop: 500 };
    let padding = 0;
    let frame: ((time: number) => void) | undefined;
    const scroll = createMessageScroll({
      measure: () => geometry,
      setPadding: (value) => { padding = value; },
      scrollTo: (value) => { geometry.scrollTop = value; },
      requestFrame: (callback) => { frame = callback; return 1; },
      cancelFrame: () => { frame = undefined; },
      reducedMotion: () => reducedMotion,
      onFollowingChange: () => {},
    });
    return { geometry, scroll, padding: () => padding, tick: (time: number) => frame?.(time) };
  }

  it("eases a sent message to one quarter and consumes space as content grows", () => {
    const f = fixture();
    f.scroll.anchor();
    f.tick(0); f.tick(160);
    expect(f.geometry.scrollTop).toBeGreaterThan(750);
    expect(f.geometry.scrollTop).toBeLessThan(1000);
    f.tick(320);
    expect(f.geometry.scrollTop).toBe(1000);
    expect(f.padding()).toBe(500);
    f.geometry.contentEnd = 1600; f.scroll.update();
    expect(f.padding()).toBe(200);
    expect(f.geometry.scrollTop).toBe(1000);
    f.geometry.contentEnd = 2000; f.scroll.update();
    expect(f.padding()).toBe(0);
    expect(f.geometry.scrollTop).toBe(1200);
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
    f.scroll.anchor(); f.tick(0); f.tick(80);
    f.scroll.pause();
    const position = f.geometry.scrollTop;
    f.tick(320);
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

});
