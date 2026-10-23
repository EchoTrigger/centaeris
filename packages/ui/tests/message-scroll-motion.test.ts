// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { createMessageScroll } from "../src/components/chat/messageScroll";

const controllers: ReturnType<typeof createMessageScroll>[] = [];
afterEach(() => { for (const controller of controllers.splice(0)) controller.dispose(); });
function fixture() {
  const geometry = { height: 800, contentEnd: 1300, anchorTop: 1200, scrollTop: 500 };
  let padding = 0;
  const scroll = createMessageScroll({
    measure: () => geometry,
    setPadding: value => { padding = value; },
    scrollTo: value => { geometry.scrollTop = value; },
    reducedMotion: () => false,
    onFollowingChange: () => {},
  });
  controllers.push(scroll);
  return { geometry, scroll, padding: () => padding };
}

test("real Motion animates toward the current anchor and then follows arriving output", async () => {
  const f = fixture();
  f.scroll.anchor();
  await vi.waitUntil(() => f.geometry.scrollTop > 500 && f.geometry.scrollTop < 1000);
  f.geometry.anchorTop = 1400; f.geometry.contentEnd = 1500;
  await vi.waitUntil(() => f.geometry.scrollTop === 1200);
  expect(f.padding()).toBe(500);
  f.geometry.contentEnd = 2400;
  f.scroll.update();
  expect(f.padding()).toBe(0);
  expect(f.geometry.scrollTop).toBe(1600);
});

test("stopping a real Motion anchor leaves the manual reading position stable", async () => {
  const f = fixture();
  f.scroll.anchor();
  await vi.waitUntil(() => f.geometry.scrollTop > 500);
  f.scroll.pause();
  f.geometry.scrollTop = 300;
  f.scroll.userScroll();
  f.geometry.contentEnd = 2000; f.scroll.update();
  await new Promise(resolve => setTimeout(resolve, 350));
  expect(f.geometry.scrollTop).toBe(300);
});
