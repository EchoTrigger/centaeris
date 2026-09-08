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
 await page.goto('/tests/fixtures/appearance.html');
 const errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 const select=page.locator('.themeToggle select');
 await select.selectOption('dark');
 await select.selectOption('light');
 await select.selectOption('dark');
 await page.waitForTimeout(600);
 await expect(page.locator('html')).toHaveAttribute('data-theme','dark');
 await expect(select).toHaveValue('dark');
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
  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);
  return document.querySelector('[data-testid="paced-stream"]').textContent.length;
 },target);
 expect(sample).toBeGreaterThan(5);
 expect(sample).toBeLessThan(target.trim().length);
 await expect(page.getByTestId('paced-stream')).toHaveText(target.trim(),{timeout:500});
 await page.evaluate(()=>window.appearanceStream('Final corrected text',false));
 await expect(page.getByTestId('paced-stream')).toHaveText('Final corrected text');
});
