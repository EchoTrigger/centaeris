import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { dirname, basename, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import { runBrowser, waitFor } from './browserHarness.mjs';

test('Session switching keeps actual sidebar DOM, scroll, disclosure and focus while cached text restores', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'centaeris-navigation-'));
  const server = await createServer({ root: fileURLToPath(new URL('../..', import.meta.url)), server: { port: 0, strictPort: false, host: '127.0.0.1' } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/session-navigation-continuity.html`, { width: 1280, height: 900 }, profile, async ({ evaluate, frame, press, send }) => {
      await waitFor(() => evaluate('window.sessionNavigationFixture?.ready()'), 'actual workspace route');
      await evaluate("sessionNavigationFixture.hydrate('session-4')");
      await waitFor(() => evaluate("document.body.textContent.includes('Readable session-4')"), 'first readable conversation');
      await evaluate(`(() => {
        const scroll = document.querySelector('.shScroll');
        const project = document.querySelector('.shProjectDisclosure');
        const row = document.querySelector('[data-session-id="session-20"]');
        project.open = false; scroll.scrollTop = 210; row.focus({preventScroll:true});
        window.retainedNavigation = {scroll, project, row, top:scroll.scrollTop, changes:0, blank:false};
        const observer = new MutationObserver(records => { retainedNavigation.changes += records.filter(record=>record.type==='childList').length; });
        observer.observe(scroll,{subtree:true,childList:true});
        window.navigationObserver=observer;
        window.observeNavigationFrame=()=> { retainedNavigation.blank ||= !document.querySelector('[data-session-id="session-20"]'); requestAnimationFrame(observeNavigationFrame); };
        requestAnimationFrame(observeNavigationFrame);
      })()`);
      await evaluate("sessionNavigationFixture.navigate('session-5')");
      await waitFor(() => evaluate("sessionNavigationFixture.connected('session-5')"), 'next Session transport');
      await frame();
      await evaluate("sessionNavigationFixture.hydrate('session-5')");
      await waitFor(() => evaluate("document.body.textContent.includes('Readable session-5')"), 'next conversation');
      await evaluate("sessionNavigationFixture.repeatNavigation('session-5'); sessionNavigationFixture.live('session-5')");
      await frame();
      const changesBefore = await evaluate('retainedNavigation.changes');
      await evaluate("sessionNavigationFixture.navigate('session-4')");
      await waitFor(() => evaluate("sessionNavigationFixture.connected('session-4')"), 'cached Session transport');
      await frame();
      const state = await evaluate(`({ sameScroll:retainedNavigation.scroll===document.querySelector('.shScroll'),
        sameProject:retainedNavigation.project===document.querySelector('.shProjectDisclosure'),
        sameRow:retainedNavigation.row===document.querySelector('[data-session-id="session-20"]'),
        folded:!retainedNavigation.project.open, scroll:retainedNavigation.scroll.scrollTop, top:retainedNavigation.top,
        focused:document.activeElement===retainedNavigation.row, blank:retainedNavigation.blank,
        cached:document.body.textContent.includes('Readable session-4'), changes:retainedNavigation.changes,
        listReads:sessionNavigationFixture.requests.filter(path=>path==='/api/workspaces/workspace/sessions').length,
        projectReads:sessionNavigationFixture.requests.filter(path=>path.endsWith('/session-projects')).length,
        failures:sessionNavigationFixture.failures })`);
      assert.deepEqual({ ...state, changes:changesBefore, scroll:state.top }, { sameScroll:true,sameProject:true,sameRow:true,folded:true,
        scroll:state.top,top:state.top,focused:true,blank:false,cached:true,changes:changesBefore,listReads:1,projectReads:1,failures:[] });
      assert.equal(state.scroll,state.top); assert.equal(state.changes,changesBefore,'selection does not rebuild directory contents');

      const switches = await evaluate(`(async () => {
        const results = [];
        for (let index=0; index<50; index++) {
          const id = index%2===0 ? 'session-5' : 'session-4';
          const other = index%2===0 ? 'session-4' : 'session-5';
          const start = performance.now();
          await sessionNavigationFixture.navigate(id);
          // The second animation frame bounds a completed readable paint;
          // this measures the rendered view rather than the navigation receipt.
          await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
          results.push({milliseconds:performance.now()-start,
            readable:document.body.textContent.includes('Readable '+id),
            crossed:document.body.textContent.includes('Readable '+other),
            same:retainedNavigation.scroll===document.querySelector('.shScroll') && retainedNavigation.row===document.querySelector('[data-session-id="session-20"]'),
            scroll:retainedNavigation.scroll.scrollTop, folded:!retainedNavigation.project.open });
        }
        return {results,changes:retainedNavigation.changes,blank:retainedNavigation.blank,
          reads:sessionNavigationFixture.requests.filter(path=>path==='/api/workspaces/workspace/sessions'||path.endsWith('/session-projects')).length};
      })()`);
      assert.equal(switches.results.length,50);
      assert.ok(switches.results.every(item=>item.readable && !item.crossed && item.same && item.folded && item.scroll===state.top));
      const timings = switches.results.map(item=>item.milliseconds).sort((left,right)=>left-right);
      const p95 = timings[Math.ceil(timings.length*0.95)-1];
      assert.ok(p95<=100,`cached first readable paint upper-bound p95 ${p95.toFixed(2)}ms exceeds 100ms`);
      assert.equal(switches.changes,0); assert.equal(switches.blank,false); assert.equal(switches.reads,2);
      console.info(JSON.stringify({cachedSwitches:50,firstReadablePaintUpperBoundP95Ms:p95,directoryChildListChanges:switches.changes}));
      await evaluate('navigationObserver.disconnect()');

      await evaluate('sessionNavigationFixture.library()');
      await waitFor(() => evaluate("document.body.textContent.includes('Navigation library')"), 'library route');
      await evaluate('sessionNavigationFixture.preferences()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.workspaceSettingsSections'))"), 'settings navigation');
      await evaluate('sessionNavigationFixture.back()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.shScroll'))"), 'restored conversation navigation');
      await frame();
      assert.equal(await evaluate("document.querySelector('.shProjectDisclosure').open"), false);
      assert.equal(await evaluate("document.querySelector('.shScroll').scrollTop"), state.top);
      await evaluate('sessionNavigationFixture.back()');
      await waitFor(() => evaluate("document.querySelector('[aria-current=page][data-session-id]')?.dataset.sessionId === 'session-4'"), 'selected Session after Back');
      await evaluate("document.querySelector('.workspaceSidebarToggle').click()");
      await waitFor(() => evaluate("document.querySelector('.workspaceSidebarToggle').getAttribute('aria-expanded')==='false'"), 'desktop sidebar hidden');
      await evaluate('window.navigationBeforeReload=true');
      await send('Page.reload', { ignoreCache: true });
      await waitFor(() => evaluate('!window.navigationBeforeReload && window.sessionNavigationFixture?.ready()').catch(() => false), 'fresh workspace after reload');
      await waitFor(() => evaluate("Boolean(document.querySelector('[data-session-id=\"session-20\"]'))"), 'reloaded directory');
      assert.equal(await evaluate("document.querySelector('.workspaceSidebarToggle').getAttribute('aria-expanded')"), 'false');
      assert.equal(await evaluate("document.querySelector('.shProjectDisclosure').open"), false);
      await evaluate("document.querySelector('.workspaceSidebarToggle').click()");
      await waitFor(() => evaluate("document.querySelector('.shScroll').scrollTop > 0"), 'restored sidebar scroll after opening');
      await frame();
      assert.equal(await evaluate("document.querySelector('.shScroll').scrollTop"), state.top);
      await evaluate("document.querySelector('.shProjectSummary').focus()");
      await press('Enter');
      await waitFor(() => evaluate("document.querySelector('.shProjectDisclosure').open"), 'native keyboard opens project');
      await press(' ', 'Space');
      await waitFor(() => evaluate("!document.querySelector('.shProjectDisclosure').open"), 'native keyboard closes project');

      await evaluate('sessionNavigationFixture.trash()');
      await waitFor(()=>evaluate("Boolean(document.querySelector('[aria-label=\"Permanently delete Deleted material\"]'))"),'Recently Deleted material');
      await evaluate(`(() => { window.purgeOpener=document.querySelector('[aria-label="Permanently delete Deleted material"]'); purgeOpener.focus(); })()`);
      await press('Enter');
      await waitFor(()=>evaluate("Boolean(document.querySelector('[role=\"dialog\"]'))"),'permanent deletion confirmation');
      const dialog = await evaluate(`(() => { const panel=document.querySelector('[role="dialog"]'), style=getComputedStyle(panel); return {
        title:panel.querySelector('h2').textContent, focused:panel.contains(document.activeElement),
        borders:[style.borderTopWidth,style.borderRightWidth,style.borderBottomWidth,style.borderLeftWidth] }; })()`);
      assert.equal(dialog.title,'Delete this material?'); assert.equal(dialog.focused,true);
      assert.ok(dialog.borders.every(width=>width==='0px'),'Recently Deleted confirmation has no panel outline');
      if (process.env.NAVIGATION_SCREENSHOT_PATH) {
        const shot=await send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
        await writeFile(process.env.NAVIGATION_SCREENSHOT_PATH,Buffer.from(shot.data,'base64'));
      }
      await press('Escape');
      await waitFor(()=>evaluate("!document.querySelector('[role=\"dialog\"]')"),'cancelled confirmation');
      assert.equal(await evaluate('document.activeElement===purgeOpener'),true,'cancel returns keyboard focus to the initiating action');
      assert.deepEqual(await evaluate('sessionNavigationFixture.failures'),[]);
    });
  } finally {
    await server.close();
    const actual = await realpath(profile);
    assert.equal(dirname(actual), await realpath(tmpdir())); assert.ok(basename(actual).startsWith('centaeris-navigation-'));
    await rm(actual, { recursive:true, force:true });
  }
});

test('Recently Deleted confirmation retains borderless styling and keyboard focus on a dark mobile viewport', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'centaeris-navigation-'));
  const server = await createServer({ root:fileURLToPath(new URL('../..',import.meta.url)),server:{port:0,strictPort:false,host:'127.0.0.1'} });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/session-navigation-continuity.html?theme=dark`,
      {width:390,height:844,theme:'dark',reduced:true},profile,async({evaluate,press,send})=>{
        await waitFor(()=>evaluate('window.sessionNavigationFixture?.ready()'),'mobile workspace route');
        assert.equal(await evaluate("document.querySelector('.workspaceDrawer').open"), false);
        await evaluate("document.querySelector('button[aria-label=\"Show sidebar\"]').click()");
        await waitFor(()=>evaluate("document.querySelector('.workspaceDrawer').open"),'navigation drawer opens');
        await waitFor(()=>evaluate("Boolean(document.querySelector('.shProjectSummary'))"),'mobile project rows');
        await evaluate("document.querySelector('.shProjectSummary').focus()");
        await press('Enter');
        await waitFor(()=>evaluate("!document.querySelector('.shProjectDisclosure').open"),'mobile project folds');
        const top = await evaluate("(() => { const scroll=document.querySelector('.shScroll'); scroll.scrollTop=200; scroll.dispatchEvent(new Event('scroll')); return scroll.scrollTop; })()");
        assert.ok(top > 0);
        await evaluate('sessionNavigationFixture.library()');
        await waitFor(()=>evaluate("!document.querySelector('.workspaceDrawer').open"),'navigation closes transient drawer');
        await evaluate("document.querySelector('button[aria-label=\"Show sidebar\"]').click()");
        await waitFor(()=>evaluate("document.querySelector('.workspaceDrawer').open && document.querySelector('.shScroll').scrollTop > 0"),'drawer position restores');
        assert.equal(await evaluate("document.querySelector('.shScroll').scrollTop"), top);
        assert.equal(await evaluate("document.querySelector('.shProjectDisclosure').open"), false);
        await press('Escape');
        await evaluate('window.navigationBeforeReload=true');
        await send('Page.reload',{ignoreCache:true});
        await waitFor(()=>evaluate('!window.navigationBeforeReload && window.sessionNavigationFixture?.ready()').catch(()=>false),'mobile reload');
        assert.equal(await evaluate("document.querySelector('.workspaceDrawer').open"), false);
        await send('Emulation.setDeviceMetricsOverride',{width:1280,height:900,deviceScaleFactor:1,mobile:false});
        await waitFor(()=>evaluate("Boolean(document.querySelector('.workspaceSidebarToggle'))"),'desktop sidebar control');
        assert.equal(await evaluate("document.querySelector('.workspaceSidebarToggle').getAttribute('aria-expanded')"),'true');
        await send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
        await waitFor(()=>evaluate("Boolean(document.querySelector('.workspaceDrawer')) && !document.querySelector('.workspaceDrawer').open"),'compact drawer remains closed');
        await evaluate('sessionNavigationFixture.trash()');
        await waitFor(()=>evaluate("Boolean(document.querySelector('[aria-label=\"Permanently delete Deleted material\"]'))"),'deleted material action');
        await evaluate(`(() => { window.purgeOpener=document.querySelector('[aria-label="Permanently delete Deleted material"]'); purgeOpener.focus(); })()`);
        await press('Enter');
        await waitFor(()=>evaluate("Boolean(document.querySelector('[role=\"dialog\"]'))"),'mobile deletion confirmation');
        const result=await evaluate(`(() => { const panel=document.querySelector('[role="dialog"]'),style=getComputedStyle(panel),bounds=panel.getBoundingClientRect(); return {
          theme:document.documentElement.dataset.theme,focused:panel.contains(document.activeElement),fits:bounds.left>=0&&bounds.right<=innerWidth,
          borders:[style.borderTopWidth,style.borderRightWidth,style.borderBottomWidth,style.borderLeftWidth]}; })()`);
        assert.equal(result.theme,'dark'); assert.equal(result.focused,true); assert.equal(result.fits,true);
        assert.ok(result.borders.every(width=>width==='0px'));
        if(process.env.NAVIGATION_SCREENSHOT_PATH){const shot=await send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
          await writeFile(process.env.NAVIGATION_SCREENSHOT_PATH,Buffer.from(shot.data,'base64'));}
        await press('Escape');
        await waitFor(()=>evaluate("!document.querySelector('[role=\"dialog\"]')"),'mobile cancellation');
        assert.equal(await evaluate('document.activeElement===purgeOpener'),true);
        assert.deepEqual(await evaluate('sessionNavigationFixture.failures'),[]);
      });
  } finally {
    await server.close();const actual=await realpath(profile);
    assert.equal(dirname(actual),await realpath(tmpdir()));assert.ok(basename(actual).startsWith('centaeris-navigation-'));
    await rm(actual,{recursive:true,force:true});
  }
});


test('docked chat tabs preserve drafts, open one window and reserve space on settings only while needed', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'centaeris-navigation-'));
  const server = await createServer({ root: fileURLToPath(new URL('../..', import.meta.url)), server: { port: 0, host: '127.0.0.1' } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/session-navigation-continuity.html`, { width: 1280, height: 900 }, profile, async ({ evaluate, frame, send }) => {
      const click = selector => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
      const padding = selector => evaluate(`parseFloat(getComputedStyle(document.querySelector(${JSON.stringify(selector)})).paddingBottom)`);
      await waitFor(() => evaluate('window.sessionNavigationFixture?.ready()'), 'workspace');
      await evaluate("sessionNavigationFixture.hydrate('session-4')");
      await waitFor(() => evaluate("document.body.textContent.includes('Readable session-4')"), 'first chat');
      const originalPadding = await padding('.workspaceWorkbench');
      await evaluate("document.querySelector('#messageDraft').focus()");
      await send('Input.insertText', { text: 'Draft for chat four' });
      await click('[aria-label="Move to toolbar"]');
      await waitFor(() => evaluate("document.querySelectorAll('[role=tab]').length === 1 && Boolean(document.querySelector('.isChatFloating'))"), 'first tab');
      const dockPadding = await padding('.workspaceWorkbench');
      assert(dockPadding > originalPadding);
      await evaluate("sessionNavigationFixture.navigate('session-5')");
      await waitFor(() => evaluate("sessionNavigationFixture.connected('session-5')"), 'second chat');
      await evaluate("sessionNavigationFixture.hydrate('session-5')");
      await waitFor(() => evaluate("document.body.textContent.includes('Readable session-5')"), 'second content');
      await evaluate("document.querySelector('#messageDraft').focus()");
      await send('Input.insertText', { text: 'Draft for chat five' });
      await click('[aria-label="Move to toolbar"]');
      await waitFor(() => evaluate("document.querySelectorAll('[role=tab]').length === 2"), 'two tabs');
      assert(await evaluate(`(() => {
        const tab = document.querySelector('.conversationTabItem:last-child [role=tab]');
        const label = tab.querySelector('span');
        return label.scrollWidth > label.clientWidth && tab.title === label.textContent
          && tab.getBoundingClientRect().width < innerWidth / 2;
      })()`), 'a long title stays bounded while its full text remains available');
      await evaluate('sessionNavigationFixture.task()');
      await waitFor(() => evaluate("Array.from(document.querySelectorAll('button')).some(button => button.textContent.trim() === 'Continue')"), 'sample task');
      await evaluate("Array.from(document.querySelectorAll('button')).find(button => button.textContent.trim() === 'Continue').click()");
      await waitFor(() => evaluate("Boolean(document.querySelector('.workFloatingConversation')) && !document.querySelector('.isChatFloating')"), 'sample takes the window');
      await click('.conversationTabItem:first-child [role=tab]'); await frame();
      assert.equal(await evaluate("document.querySelectorAll('.workFloatingConversation').length"), 0);
      await waitFor(() => evaluate("document.querySelector('#messageDraft').value === 'Draft for chat four'"), 'first draft restored');
      assert.equal(await evaluate("document.querySelectorAll('.isChatFloating .workspaceChatColumn').length"), 1);
      await click('.conversationTabItem:last-child [role=tab]'); await frame();
      await waitFor(() => evaluate("document.querySelector('#messageDraft').value === 'Draft for chat five'"), 'second draft restored');
      await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false }); await frame();
      assert(await evaluate("(() => { const r = document.querySelector('.conversationDock').getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth; })()"));
      await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false }); await frame();
      await click('.conversationTabItem:last-child [role=tab]');
      await waitFor(() => evaluate("!document.querySelector('.isChatFloating')"), 'minimized window');
      assert.equal(await evaluate("document.querySelectorAll('[role=tab]').length"), 2);
      await evaluate('sessionNavigationFixture.preferences()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.workspaceSettingsContent'))"), 'settings');
      assert.equal(await padding('.workspaceSettingsPage'), dockPadding);
      await click('.conversationTabItem:first-child [role=tab]');
      await waitFor(() => evaluate("Boolean(document.querySelector('.isChatFloating')) && Boolean(document.querySelector('.workspaceSettingsPage'))"), 'tab opens from settings');
      assert.equal(await evaluate('sessionNavigationFixture.pathname()'), '/w/workspace/settings/preferences');
      const tabPoint = await evaluate("(() => { const r = document.querySelector('[role=tab]').getBoundingClientRect(); return { x:r.x + r.width/2, y:r.y + r.height/2 }; })()");
      for (let clickIndex = 0; clickIndex < 2; clickIndex++) {
        await send('Input.dispatchMouseEvent', { type:'mousePressed', button:'left', clickCount:1, ...tabPoint });
        await send('Input.dispatchMouseEvent', { type:'mouseReleased', button:'left', clickCount:1, ...tabPoint });
        await frame();
      }
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 10, y: 10 }); await frame();
      const tabStyle = () => evaluate(`(() => {
        const item = document.querySelector('.conversationTabItem');
        const title = item.querySelector('[role=tab] > span');
        const close = item.querySelector('.conversationTabClose');
        const rect = item.getBoundingClientRect();
        return { width: rect.width, x: rect.x + rect.width / 2, y: rect.y + rect.height / 2,
          closeOpacity: getComputedStyle(close).opacity, mask: getComputedStyle(title).maskImage,
          textOverflow: getComputedStyle(title).textOverflow, fullTitle: title.clientWidth >= title.scrollWidth,
          shadow: getComputedStyle(item).boxShadow, background: getComputedStyle(item).backgroundColor };
      })()`);
      const resting = await tabStyle();
      assert.equal(resting.closeOpacity, '0'); assert.equal(resting.mask, 'none'); assert(resting.fullTitle);
      assert.equal(resting.shadow, 'none'); assert.equal(resting.background, 'rgba(0, 0, 0, 0)');
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: resting.x, y: resting.y }); await frame();
      const hovering = await tabStyle();
      assert.equal(hovering.closeOpacity, '1'); assert.notEqual(hovering.mask, 'none');
      assert.equal(hovering.textOverflow, 'clip'); assert.equal(hovering.width, resting.width);
      if (process.env.NAVIGATION_SCREENSHOT_PATH) {
        const shot = await send('Page.captureScreenshot', { format: 'png' });
        await writeFile(process.env.NAVIGATION_SCREENSHOT_PATH, Buffer.from(shot.data, 'base64'));
      }

      await evaluate('sessionNavigationFixture.reports()');
      await waitFor(() => evaluate("Boolean(document.querySelector('[data-report-page]'))"), 'another standalone page');
      await click('.conversationTabItem:last-child [role=tab]');
      await waitFor(() => evaluate("document.querySelector('#messageDraft').value === 'Draft for chat five'"), 'chat on another page');
      assert.equal(await evaluate('sessionNavigationFixture.pathname()'), '/w/workspace/reports');
      assert.equal(await evaluate("document.querySelector('[aria-label=\"Report filter\"]').value"), 'Keep this filter');
      await evaluate('sessionNavigationFixture.preferences()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.workspaceSettingsContent'))"), 'back to settings');
      await click('.conversationTabItem:first-child [role=tab]');
      await waitFor(() => evaluate("document.querySelector('[role=tab][aria-selected=true]')?.textContent === 'Conversation 4'"), 'first chat selected');
      await click('.conversationTabItem:last-child .conversationTabClose');
      await waitFor(() => evaluate("document.querySelectorAll('[role=tab]').length === 1"), 'inactive tab closed');
      assert(await evaluate("Boolean(document.querySelector('.isChatFloating'))"));
      await click('[aria-label="Close chat"]');
      await waitFor(() => evaluate("!document.querySelector('[role=tab]') && !document.querySelector('.isChatFloating')"), 'active window closed');
      assert.equal(await evaluate('sessionNavigationFixture.pathname()'), '/w/workspace/settings/preferences');
      assert.equal(await padding('.workspaceSettingsPage'), originalPadding);
      assert.deepEqual(await evaluate('sessionNavigationFixture.mutations'), []);
      // Closing tabs preserves the original chats; reopen one through its existing route.
      await evaluate("sessionNavigationFixture.navigate('session-5')");
      await waitFor(() => evaluate("document.querySelector('.conversationSwitcher')?.textContent.includes('Conversation 5')"), 'closed chat still available');
      await click('[aria-label="Move to toolbar"]');
      await waitFor(() => evaluate("document.querySelector('[role=tab][aria-selected=true]')?.textContent.startsWith('Conversation 5 ')"), 'reopened chat');

      await click('[aria-label="Expand chat"]');
      await waitFor(() => evaluate("!document.querySelector('[role=tab]')"), 'all expanded');
      assert.equal(await padding('.workspaceWorkbench'), originalPadding);
      await evaluate('sessionNavigationFixture.preferences()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.workspaceSettingsContent'))"), 'settings without dock');
      assert.equal(await padding('.workspaceSettingsPage'), originalPadding);
      assert.deepEqual(await evaluate('sessionNavigationFixture.failures'), []);
    });
  } finally {
    await server.close(); const actual = await realpath(profile);
    assert.equal(dirname(actual), await realpath(tmpdir())); assert.ok(basename(actual).startsWith('centaeris-navigation-'));
    await rm(actual, { recursive: true, force: true });
  }
});


test('navigation groups keep favorites between personal and workspace entries and hide empty favorites', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'centaeris-navigation-'));
  const server = await createServer({ root: fileURLToPath(new URL('../..', import.meta.url)), server: { port: 0, host: '127.0.0.1' } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/session-navigation-continuity.html`, { width: 1280, height: 900 }, profile, async ({ evaluate, frame, send }) => {
      await waitFor(() => evaluate('window.sessionNavigationFixture?.ready()'), 'workspace');
      await evaluate("sessionNavigationFixture.hydrate('session-4')");
      await evaluate('sessionNavigationFixture.task()');
      await waitFor(() => evaluate("Boolean(document.querySelector('.workPrototype'))"), 'work page');
      const labels = await evaluate("Array.from(document.querySelector('.workPrimaryNav').querySelectorAll('a,summary')).map(el=>el.textContent.trim())");
      assert.deepEqual(labels, ['Inbox', 'My issues', 'Reviews', 'Agent', 'Favorites', 'KKnowledge discovery improvements', 'Home', 'Issues', 'Projects', 'Views', 'Extensions', 'Library']);
      assert(await evaluate("(() => {const nav=document.querySelector('.workPrimaryNav'); return nav.scrollWidth <= nav.clientWidth;})()"), 'long favorite titles fit the navigation');
      const favoriteVisible = () => evaluate("document.querySelector('.shFavorites a')?.checkVisibility() ?? false");
      assert(await favoriteVisible());
      await evaluate("document.querySelector('.shFavorites summary').click()"); await frame();
      assert.equal(await favoriteVisible(), false);
      await evaluate("document.querySelector('.shFavorites summary').click()"); await frame();
      assert(await favoriteVisible());
      await evaluate("document.querySelector('.shFavorites a').click()");
      await waitFor(() => evaluate(`Boolean(document.querySelector('[aria-label="Unfavorite project"]'))`), 'favorite project');
      await evaluate(`document.querySelector('[aria-label="Unfavorite project"]').click()`);
      await waitFor(() => evaluate("!document.querySelector('.shFavorites')"), 'empty favorites hidden');
      await evaluate(`document.querySelector('[aria-label="Favorite project"]').click()`);
      await waitFor(() => evaluate("Boolean(document.querySelector('.shFavorites'))"), 'favorites restored');
      for (const title of ['Inbox', 'My issues', 'Reviews', 'Home', 'Issues', 'Projects', 'Views']) {
        await evaluate(`Array.from(document.querySelectorAll('.workPrimaryNav a')).find(el=>el.textContent.trim()===${JSON.stringify(title)}).click()`);
        await waitFor(() => evaluate(`document.querySelector('.workHeader h1')?.textContent.startsWith(${JSON.stringify(title)})`), title);
        assert.equal(await evaluate("document.querySelector('.workPrimaryNav a[aria-current=page]')?.textContent.trim()"), title);
      }
      await evaluate("Array.from(document.querySelectorAll('.workPrimaryNav a')).find(el=>el.textContent.trim()==='My issues').click()");
      await frame();
      if (process.env.NAVIGATION_SCREENSHOT_PATH) {
        const shot = await send('Page.captureScreenshot', { format: 'png' });
        await writeFile(process.env.NAVIGATION_SCREENSHOT_PATH, Buffer.from(shot.data, 'base64'));
      }
      assert.deepEqual(await evaluate('sessionNavigationFixture.failures'), []);
    });
  } finally {
    await server.close(); const actual = await realpath(profile);
    assert.equal(dirname(actual), await realpath(tmpdir())); assert.ok(basename(actual).startsWith('centaeris-navigation-'));
    await rm(actual, { recursive: true, force: true });
  }
});
