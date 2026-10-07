// Wait for the measured layout to settle, including grid/sidebar transitions.
// Two animation frames alone can capture a value partway through a resize.
export async function waitForLayout(selectors: string[]) {
  await document.fonts.ready;
  const deadline = performance.now() + 5000;
  let previous = "";
  let stableSince = performance.now();
  while (performance.now() < deadline) {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
    const current = JSON.stringify(selectors.flatMap(selector => [...document.querySelectorAll(selector)].flatMap(node => {
      const bounds = node.getBoundingClientRect();
      return [bounds.left, bounds.top, bounds.width, bounds.height].map(value => Math.round(value * 100) / 100);
    })));
    if (current !== previous) { previous = current; stableSince = performance.now(); }
    else if (performance.now() - stableSince >= 250) return;
  }
  throw new Error("Synthetic conversation geometry did not settle");
}
