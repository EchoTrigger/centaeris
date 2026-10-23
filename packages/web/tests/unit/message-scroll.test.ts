import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createMessageScroll } from "../../src/chat/messageScroll.ts";
import { createMessageScroll as createDesktopMessageScroll } from "../../../ui/src/components/chat/messageScroll.ts";

describe("message anchoring", () => {
  function fixture(reducedMotion = false) {
    const geometry = { height: 800, contentEnd: 1300, anchorTop: 1200, scrollTop: 500 };
    let padding = 0;
    let frame: Function | undefined;
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
    assert.ok(f.geometry.scrollTop > 750);
    assert.ok(f.geometry.scrollTop < 1000);
    f.tick(320);
    assert.equal(f.geometry.scrollTop, 1000);
    assert.equal(f.padding(), 500);
    f.geometry.contentEnd = 1600; f.scroll.update();
    assert.equal(f.padding(), 200);
    assert.equal(f.geometry.scrollTop, 1000);
    f.geometry.contentEnd = 2000; f.scroll.update();
    assert.equal(f.padding(), 0);
    assert.equal(f.geometry.scrollTop, 1200);
  });

  it("reclaims blank space while reading upward without moving the reading position", () => {
    const f = fixture(true);
    f.scroll.anchor(); f.scroll.pause();
    f.geometry.scrollTop = 700; f.scroll.userScroll();
    assert.equal(f.padding(), 200);
    assert.equal(f.geometry.scrollTop, 700);
    f.geometry.scrollTop = 400; f.scroll.userScroll();
    assert.equal(f.padding(), 0);
    f.geometry.contentEnd = 1800; f.scroll.update();
    assert.equal(f.geometry.scrollTop, 400);
  });

  it("cancels animation on user intent and resets space on session change", () => {
    const f = fixture();
    f.scroll.anchor(); f.tick(0); f.tick(80);
    f.scroll.pause();
    const position = f.geometry.scrollTop;
    f.tick(320);
    assert.equal(f.geometry.scrollTop, position);
    f.scroll.reset();
    assert.equal(f.padding(), 0);
  });
});


// Both hosts share reading and anchoring semantics; animation adapters may differ.
for (const [host, create] of [["Web", createMessageScroll], ["Desktop", createDesktopMessageScroll]] as const) {
  describe(`${host} message scrolling contract`, () => {
    function fixture() {
      const geometry = { height: 800, contentEnd: 1300, anchorTop: 1200, scrollTop: 500 };
      let padding = 0;
      let following = true;
      const scroll = create({
        measure: () => geometry,
        setPadding: value => { padding = value; },
        scrollTo: value => { geometry.scrollTop = value; },
        requestFrame: () => { throw new Error("Reduced motion must not schedule animation"); },
        cancelFrame: () => {},
        reducedMotion: () => true,
        onFollowingChange: value => { following = value; },
      });
      return { geometry, scroll, padding: () => padding, following: () => following };
    }

    it("anchors the sent prompt, consumes reserved space and follows arriving content", () => {
      const f = fixture();
      f.scroll.anchor();
      assert.equal(f.geometry.scrollTop, 1000);
      assert.equal(f.padding(), 500);
      assert.equal(f.following(), true);
      f.geometry.contentEnd = 1600; f.scroll.update();
      assert.equal(f.geometry.scrollTop, 1000);
      assert.equal(f.padding(), 200);
      f.geometry.contentEnd = 2000; f.scroll.update();
      assert.equal(f.geometry.scrollTop, 1200);
      assert.equal(f.padding(), 0);
      f.scroll.dispose();
    });

    it("preserves explicit manual reading until return to latest", () => {
      const f = fixture();
      f.scroll.anchor(); f.scroll.pause();
      f.geometry.scrollTop = 700; f.scroll.userScroll();
      assert.equal(f.padding(), 200);
      f.geometry.scrollTop = 400; f.scroll.userScroll();
      assert.equal(f.padding(), 0);
      f.geometry.contentEnd = 1800; f.scroll.update();
      assert.equal(f.following(), false);
      assert.equal(f.geometry.scrollTop, 400);
      f.scroll.jump();
      assert.equal(f.following(), true);
      assert.equal(f.geometry.scrollTop, 1000);
      f.scroll.dispose();
    });

    it("retargets resized content and clears reserved space on conversation reset", () => {
      const f = fixture();
      f.geometry.contentEnd = 500; f.geometry.anchorTop = 400;
      f.scroll.anchor();
      assert.equal(f.geometry.scrollTop, 200);
      assert.equal(f.padding(), 500);
      f.geometry.height = 400;
      f.scroll.update();
      assert.equal(f.geometry.scrollTop, 300);
      assert.equal(f.padding(), 200);
      f.scroll.reset();
      assert.equal(f.padding(), 0);
      assert.equal(f.following(), true);
      f.scroll.update();
      assert.equal(f.geometry.scrollTop, 100);
      f.scroll.dispose();
    });
  });
}
