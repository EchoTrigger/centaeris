const {test,expect}=require('@playwright/test');
test('short reasoning preview stays beside its title and hover changes text only',async({page})=>{
 await page.goto('/tests/fixtures/reasoning.html');
 const preview=page.locator('.reasoningPreviewText').first();
 const title=preview.locator('xpath=../preceding-sibling::span');
 const p=await preview.boundingBox(),t=await title.boundingBox();
 expect(p.x-(t.x+t.width)).toBeLessThan(24);
 const toggle=page.locator('.agentReasoningSummary').first();
 await toggle.hover();
 await expect(toggle).toHaveCSS('background-color','rgba(0, 0, 0, 0)');
 await expect(toggle).toHaveCSS('color','rgb(32, 36, 40)');
 const tool=page.locator('.agent-operation-summary-toggle').first();
 await tool.hover();
 await expect(tool).toHaveCSS('background-color','rgba(0, 0, 0, 0)');
 await expect(tool.locator('.agent-operation-summary-text')).toHaveCSS('color','rgb(32, 36, 40)');
});

test('rapid theme choices settle on the last selection',async({page})=>{
 await page.emulateMedia({colorScheme:'light'});
 await page.goto('/tests/fixtures/appearance.html');
 const errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 await expect(page.locator('.themeToggle select')).toHaveCount(0);
 await page.getByRole('button',{name:'Switch to dark theme'}).click();
 await page.getByRole('button',{name:'Switch to light theme'}).click();
 await page.getByRole('button',{name:'Switch to dark theme'}).click();
 await page.waitForTimeout(600);
 await expect(page.locator('html')).toHaveAttribute('data-theme','dark');
 await expect(page.getByRole('button',{name:'Switch to light theme'})).toBeVisible();
 expect(errors).toEqual([]);
});

test('reduced motion and history display complete text without pacing',async({page})=>{
 await page.emulateMedia({reducedMotion:'reduce'});
 await page.goto('/tests/fixtures/appearance.html');
 for(const live of [true,false]) {
  const text='完整内容 😀 '.repeat(100).trim();
  const shown=await page.evaluate(async({text,live})=>{
   window.appearanceStream(text,live);
   await new Promise(requestAnimationFrame);
   await new Promise(requestAnimationFrame);
   return document.querySelector('[data-testid="paced-stream"]').textContent;
  },{text,live});
  expect(shown).toBe(text);
 }
});

test('theme icon sweeps, persists, and reduced motion skips the sweep', async ({page})=>{
 await page.emulateMedia({colorScheme:'light'});
 await page.goto('/tests/fixtures/appearance.html');
 await page.getByRole('button',{name:'Switch to dark theme'}).click();
 await expect(page.locator('html')).toHaveAttribute('data-theme','dark');
 await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content','#242424');
 await expect.poll(()=>page.evaluate(()=>document.getAnimations().some(animation=>
  animation.effect?.getTiming().duration===520 && animation.effect.getKeyframes().some(frame=>String(frame.clipPath).startsWith('circle('))
 ))).toBe(true);
 await page.waitForTimeout(600);
 await page.reload();
 await expect(page.getByRole('button',{name:'Switch to light theme'})).toBeVisible();
 await page.emulateMedia({reducedMotion:'reduce'});
 await page.getByRole('button',{name:'Switch to light theme'}).click();
 await expect(page.locator('html')).toHaveAttribute('data-theme','light');
 expect(await page.evaluate(()=>document.getAnimations().filter(a=>a.effect?.pseudoElement?.includes('view-transition')).length)).toBe(0);
});

test('fast live batches are spread over frames and terminal content is exact',async({page})=>{
 await page.goto('/tests/fixtures/appearance.html');
 await page.waitForTimeout(150);
 const target='Start '+ '自然流式输出 '.repeat(150);
 const sample=await page.evaluate(async text=>{
  window.appearanceStream(text);
  let length=5;
  for(let frame=0;frame<20 && length===5;frame++) {
   await new Promise(requestAnimationFrame);
   length=document.querySelector('[data-testid="paced-stream"]').textContent.length;
  }
  return length;
 },target);
 expect(sample).toBeGreaterThan(5);
 expect(sample).toBeLessThan(target.trim().length);
 await expect(page.getByTestId('paced-stream')).toHaveText(target.trim(),{timeout:500});
 await page.evaluate(()=>window.appearanceStream('Final corrected text',false));
 await expect(page.getByTestId('paced-stream')).toHaveText('Final corrected text');
});
test("live reasoning follows latest, detaches on scrolling, and resumes at bottom or reopening", async ({ page }) => {
  await page.goto("/tests/fixtures/reasoning.html");
  const content = (count) => Array.from({ length: count }, (_, i) => `Paragraph ${i + 1}: inspect inputs and constraints.`).join("\n\n");
  await page.evaluate((text) => window.reasoningFixture.liveSnapshot(1, text), content(40));
  const toggle = page.locator(".agentReasoningSummary").first();
  await toggle.click();
  const body = page.locator(".agentReasoningBody");
  const gap = () => body.evaluate((node) => node.scrollHeight - node.clientHeight - node.scrollTop);
  await expect.poll(gap).toBeLessThanOrEqual(2);
  await page.evaluate((text) => window.reasoningFixture.liveSnapshot(2, text), content(50));
  await expect(body).toContainText("Paragraph 50:");
  await expect.poll(gap).toBeLessThanOrEqual(2);
  await body.hover();
  await page.mouse.wheel(0, -250);
  await expect.poll(gap).toBeGreaterThan(100);
  // Wait for the wheel gesture to settle before checking that new text preserves position.
  await page.waitForTimeout(200);
  const detachedTop = await body.evaluate((node) => node.scrollTop);
  await page.evaluate((text) => window.reasoningFixture.liveSnapshot(3, text), content(60));
  await expect(body).toContainText("Paragraph 60:");
  await expect.poll(() => body.evaluate((node) => node.scrollTop)).toBe(detachedTop);
  await body.focus();
  await page.keyboard.press("Control+End");
  await expect.poll(gap).toBeLessThanOrEqual(2);
  await page.evaluate((text) => window.reasoningFixture.liveSnapshot(4, text), content(70));
  await expect(body).toContainText("Paragraph 70:");
  await expect.poll(gap).toBeLessThanOrEqual(2);
  await page.keyboard.press("Control+Home");
  await expect.poll(gap).toBeGreaterThan(100);
  await toggle.click();
  await toggle.click();
  await expect.poll(gap).toBeLessThanOrEqual(2);
});
