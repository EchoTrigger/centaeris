/* Real browser interaction checks; data is explicitly an isolated test fixture. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE);
(async () => {
  const state = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  const row = state.trials[0];
  row.startedAt = new Date(Date.now() - 9000).toISOString();
  const output = {trialId:row.trialId, sessionId:row.sessionId, agentRunId:row.agentRunId,
    sourceSequence:40, truncated:false, error:null, entries:Array.from({length:35}, (_,i) =>
      ({id:`entry-${i}`,kind:'Assistant',text:`Observed message ${i}\n${'line\n'.repeat(4)}`}))};
  state.outputs = [output];
  const browser = await chromium.launch({headless:true, executablePath:process.env.CHROME_BINARY});
  try {
    const page = await browser.newPage({viewport:{width:1440,height:1000}});
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    let disconnected = false;
    await page.route('**/api/state', route => disconnected ? route.abort() : route.fulfill({json:state}));
    await page.goto(process.argv[2]);
    const timer = page.getByLabel('Slot 1 elapsed time');
    const log = page.getByRole('log', {name:'Slot 1 output',exact:true});
    const follow = page.getByRole('button', {name:'Follow Slot 1 output',exact:true});
    await page.getByText('Observed message 34', {exact:false}).waitFor();
    assert.equal(await page.getByText(/^Last observation \d/).count(),0,'Global observation age must not be displayed');
    await page.evaluate(() => document.fonts.ready);
    const before = await timer.boundingBox();
    row.startedAt = new Date(Date.now()-360000000).toISOString();
    await page.waitForFunction(() => document.querySelector('[aria-label="Slot 1 elapsed time"]').textContent.startsWith('100:'));
    const after = await timer.boundingBox();
    assert.equal(before.width, after.width, 'Elapsed timer resized');
    assert.equal(before.x, after.x, 'Elapsed timer shifted');
    await log.focus();
    await log.evaluate(el => {el.scrollTop=0;el.dispatchEvent(new Event('scroll'));});
    await page.waitForFunction(() => document.querySelector('[aria-label="Follow Slot 1 output"]').getAttribute('aria-pressed')==='false');
    output.entries.push({id:'new',kind:'Tool',text:'<script>window.unsafe=true</script>\nnew output'});
    await page.getByText('<script>window.unsafe=true</script>\nnew output',{exact:true}).waitFor();
    assert.equal(await log.evaluate(el => el.scrollTop), 0, 'New output stole scroll position');
    assert.equal(await log.evaluate(el => document.activeElement===el), true, 'Refresh stole focus');
    assert.equal(await page.evaluate(() => window.unsafe), undefined);
    await follow.click();
    await page.waitForFunction(() => {const el=document.querySelector('[aria-label="Slot 1 output"]');return el.scrollHeight-el.clientHeight-el.scrollTop<2;});
    row.status='completed'; row.resourceHolding=0; row.finishedAt=new Date().toISOString();
    state.slots[0].trialIds=[];
    await page.getByText('Last observed trial', {exact:true}).waitFor();
    const stopped = await timer.innerText();
    await page.waitForTimeout(1200);
    assert.equal(await timer.innerText(), stopped, 'Terminal timer continued');
    assert.ok(await log.locator('article').count()<=40);
    disconnected=true;
    await page.getByText('Monitor disconnected / job status unknown',{exact:true}).waitFor();
    await page.getByText('<script>window.unsafe=true</script>\nnew output',{exact:true}).waitFor();
    disconnected=false;
    await page.reload();
    await page.getByText('Observed message 34',{exact:false}).waitFor();
    assert.equal(await timer.innerText(), stopped, 'Reload reset terminal elapsed time');
    await page.setViewportSize({width:390,height:844});
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth>window.innerWidth),false,'Page overflow');
    assert.equal(await page.evaluate(() => getComputedStyle(document.body).backgroundColor),'rgb(0, 0, 0)');
    if (process.env.MONITOR_SCREENSHOT_DIR) {
      await page.screenshot({path:process.env.MONITOR_SCREENSHOT_DIR+'/mobile.png',fullPage:true});
      await page.setViewportSize({width:1440,height:1000});
      await page.screenshot({path:process.env.MONITOR_SCREENSHOT_DIR+'/desktop.png',fullPage:true});
    }
    assert.deepEqual(errors,[]);
    await page.close();
    const response=await fetch(process.argv[2]+'/api/state');
    assert.equal(response.status,200,'Closing page affected monitor');
    console.log('Timer geometry, held scroll/focus, follow, terminal/reload, disconnect, text safety and mobile passed');
  } finally {await browser.close();}
})().catch(error => {console.error(error);process.exitCode=1;});
