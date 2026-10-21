import { expect, test, type Page } from '@playwright/test';

// The preview's pretend simulator draws a home screen at 402×874 points; nothing here
// touches Xcode. Calls are recorded so gestures can be checked in points.
const calls = (page: Page, command: string): Promise<any[]> => page.evaluate(command => ((window as any).__simCalls ?? []).filter((entry: any) => entry.command === command), command);
const inputs = async (page: Page) => (await calls(page, 'simulator_input')).map(entry => entry.args.input);

/** The tab and the preview's device list follow the host: iOS and Android on macOS, Android alone on Windows. */
async function asPlatform(page: Page, platform: 'MacIntel' | 'Win32'): Promise<void> {
  await page.addInitScript(value => Object.defineProperty(Navigator.prototype, 'platform', { get: () => value }), platform);
}
async function open(page: Page, platform: 'MacIntel' | 'Win32' = 'MacIntel'): Promise<void> {
  await asPlatform(page, platform);
  await page.route('**/src/preview.ts*', async route => {
    const response = await route.fetch();
    const source = await response.text();
    await route.fulfill({ response, body: source.replace(/switch\s*\(command\)\s*\{/, `
      window.__simEmit ??= (name, event) => emit(name, event);
      if (command.startsWith('simulator_') && command !== 'simulator_frame') (window.__simCalls ??= []).push({ command, args: structuredClone(args) });
      if (command === 'simulator_frame') (window.__simFrames = (window.__simFrames ?? 0) + 1);
      switch (command) {
    `) });
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  await expect(page.locator('.task-item.selected')).toBeVisible();
}
/** The phone tab in the right sidebar opens the simulator. */
async function showSimulator(page: Page): Promise<void> {
  if (!(await page.locator('#inspector').isVisible())) await page.locator('#inspector-toggle').click();
  await page.locator('.inspector-tabs [data-action="tab-simulator"]').click();
}
/** The display after facing forward: the screen is centered in the stage. */
async function stage(page: Page) {
  const box = (await page.locator('.sim-stage').boundingBox())!;
  return { box, center: { x: box.x + box.width / 2, y: box.y + box.height / 2 } };
}
async function faceForward(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Face forward' }).click();
  await page.waitForTimeout(700);
}

test('the simulator opens in the inspector with a live 3D device', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  const panel = page.locator('#simulator-panel');
  await expect(panel).toBeVisible();
  await expect(page.locator('.inspector-tabs [data-action="tab-simulator"]')).toHaveClass(/selected/);
  await expect(page.locator('#inspector-content')).toBeHidden();
  await expect(panel.locator('.sim-device-name')).toHaveText('iPhone 17 Pro');
  await expect(panel.locator('.sim-device-runtime')).toHaveText('iOS 26.2 · Running');
  await expect(panel.locator('canvas.sim-canvas')).toBeVisible();
  // The booted device was chosen for agents too, and frames flow.
  expect((await calls(page, 'simulator_select'))[0].args).toEqual({ udid: '3720D4A0-B8C5-4DAC-9425-6A4685BC533E' });
  await expect.poll(() => page.evaluate(() => (window as any).__simFrames ?? 0)).toBeGreaterThan(0);
  await expect(panel.locator('.sim-axe')).toBeHidden();
  // Leaving the tab stops asking for frames.
  await page.locator('.inspector-tabs [data-action="tab-changes"]').click();
  await expect(panel).toBeHidden();
  await expect(page.locator('#inspector-content')).toBeVisible();
  const before = await page.evaluate(() => (window as any).__simFrames);
  await page.waitForTimeout(1200);
  expect(await page.evaluate(() => (window as any).__simFrames)).toBeLessThanOrEqual(before + 1);
});

test('clicks and drags on the display become taps and swipes in points; elsewhere they turn the device', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  await faceForward(page);
  const { box, center } = await stage(page);
  await page.mouse.click(center.x, center.y);
  await expect.poll(async () => (await inputs(page)).length).toBe(1);
  const tap = (await inputs(page))[0];
  expect(tap.kind).toBe('tap');
  expect(Math.abs(tap.x - 201)).toBeLessThan(12);
  expect(Math.abs(tap.y - 437)).toBeLessThan(20);
  // A drag up the display is a swipe up.
  await page.mouse.move(center.x, center.y + 120); await page.mouse.down();
  await page.mouse.move(center.x, center.y - 80, { steps: 8 }); await page.mouse.up();
  await expect.poll(async () => (await inputs(page)).length).toBe(2);
  const swipe = (await inputs(page))[1];
  expect(swipe.kind).toBe('swipe');
  expect(swipe.endY).toBeLessThan(swipe.y - 100);
  // Dragging the empty stage orbits the model and sends nothing.
  await page.mouse.move(box.x + 12, box.y + 30); await page.mouse.down();
  await page.mouse.move(box.x + 160, box.y + 60, { steps: 8 }); await page.mouse.up();
  await page.waitForTimeout(300);
  expect(await inputs(page)).toHaveLength(2);
  // Turned away, the same point no longer lands where it did; facing forward restores it.
  await faceForward(page);
  await page.mouse.click(center.x, center.y);
  await expect.poll(async () => (await inputs(page)).length).toBe(3);
  expect(Math.abs((await inputs(page))[2].x - 201)).toBeLessThan(12);
});

test('typing, keys and hardware buttons reach the device', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  await faceForward(page);
  const { center } = await stage(page);
  await page.mouse.click(center.x, center.y);
  await page.keyboard.type('hello');
  await page.keyboard.press('Enter');
  await expect.poll(async () => (await inputs(page)).slice(1)).toEqual([{ kind: 'type', text: 'hello' }, { kind: 'key', key: 'return' }]);
  await page.locator('#simulator-panel').getByRole('button', { name: 'Home', exact: true }).click();
  await page.locator('#simulator-panel').getByRole('button', { name: 'Lock', exact: true }).click();
  await expect.poll(async () => (await inputs(page)).slice(3)).toEqual([{ kind: 'button', button: 'home' }, { kind: 'button', button: 'lock' }]);
  await page.locator('#simulator-panel').getByRole('button', { name: 'Toggle dark appearance' }).click();
  await expect.poll(async () => (await calls(page, 'simulator_appearance')).at(-1)?.args.mode).toBe('dark');
});

test('power, device choice and agent requests to show the simulator', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  const panel = page.locator('#simulator-panel');
  await panel.getByRole('button', { name: 'Shut down simulator' }).click();
  await expect(panel.locator('.sim-device-runtime')).toHaveText('iOS 26.2 · Shutdown');
  await expect(panel.getByRole('button', { name: 'Home', exact: true })).toBeDisabled();
  await panel.getByRole('button', { name: 'Boot simulator' }).click();
  await expect(panel.locator('.sim-device-runtime')).toHaveText('iOS 26.2 · Running');
  expect((await calls(page, 'simulator_power')).map(entry => entry.args.on)).toEqual([false, true]);
  await panel.getByRole('button', { name: 'Choose simulator' }).click();
  await page.getByRole('menuitemradio', { name: /iPad Air/ }).click();
  await expect(panel.locator('.sim-device-name')).toHaveText('iPad Air 11-inch (M3)');
  expect((await calls(page, 'simulator_select')).at(-1).args).toEqual({ udid: '5B7E2C1A-0D3F-4E8B-9A61-2F4C8D7E9B10' });
  // An agent booting a device brings the panel back.
  await page.locator('.inspector-tabs [data-action="tab-files"]').click();
  await expect(panel).toBeHidden();
  await page.evaluate(() => (window as any).__simEmit('simulator-changed', { udid: '5B7E2C1A-0D3F-4E8B-9A61-2F4C8D7E9B10', show: true }));
  await expect(panel).toBeVisible();
});

test('the device wears its launch finish and remembers another', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  const finish = page.getByRole('button', { name: /Device finish/ });
  await expect(finish).toHaveAccessibleName('Device finish: Cosmic Orange');
  await finish.click();
  await page.getByRole('menuitemradio', { name: 'Silver' }).click();
  await expect(finish).toHaveAccessibleName('Device finish: Silver');
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('pandacode.simulatorFinish') ?? '{}'))).toEqual({ 'iPhone-17-Pro': 'silver' });
  await page.reload();
  await showSimulator(page);
  await expect(page.getByRole('button', { name: /Device finish/ })).toHaveAccessibleName('Device finish: Silver');
});

test('Android emulators sit beside iOS simulators with Back and Recents', async ({ page }) => {
  await open(page);
  await showSimulator(page);
  const panel = page.locator('#simulator-panel');
  await expect(panel.getByRole('button', { name: 'Back', exact: true })).toBeHidden();
  await panel.getByRole('button', { name: 'Choose simulator' }).click();
  await page.getByRole('menuitemradio', { name: /Pixel 9/ }).click();
  await expect(panel.locator('.sim-device-runtime')).toHaveText('Android 15 · Shutdown');
  await panel.getByRole('button', { name: 'Boot simulator' }).click();
  await expect(panel.locator('.sim-device-runtime')).toHaveText('Android 15 · Running');
  expect((await calls(page, 'simulator_power')).at(-1).args).toEqual({ udid: 'android:Pixel_9_API_35', on: true });
  await panel.getByRole('button', { name: 'Back', exact: true }).click();
  await panel.getByRole('button', { name: 'Recent apps' }).click();
  await panel.getByRole('button', { name: 'Home', exact: true }).click();
  await expect.poll(async () => (await inputs(page)).map(input => input.button)).toEqual(['back', 'recents', 'home']);
  await expect(page.getByRole('button', { name: /Device finish/ })).toHaveAccessibleName('Device finish: Obsidian');
  // Touch maps to dp like iOS points.
  await faceForward(page);
  const { center } = await stage(page);
  await page.mouse.click(center.x, center.y);
  await expect.poll(async () => (await inputs(page)).length).toBe(4);
  const tap = (await inputs(page))[3];
  expect(Math.abs(tap.x - 206)).toBeLessThan(14);
  expect(Math.abs(tap.y - 457)).toBeLessThan(22);
});

test('Android can be set up in one click when the SDK is missing', async ({ page }) => {
  await asPlatform(page, 'MacIntel');
  await page.route('**/src/preview.ts*', async route => {
    const response = await route.fetch();
    const source = await response.text();
    await route.fulfill({ response, body: source.replace(/switch\s*\(command\)\s*\{/, `
      window.__simEmit ??= (name, event) => emit(name, event);
      if (command.startsWith('simulator_') && command !== 'simulator_frame') (window.__simCalls ??= []).push({ command, args: structuredClone(args) });
      const noAndroid = value => ({ ...value, android: false, devices: value.devices.filter(d => d.platform !== 'android') });
      if (command === 'simulator_status' && !window.__androidReady) return noAndroid(previewSimulator.status());
      if (command === 'simulator_select' && !window.__androidReady) return noAndroid(previewSimulator.select(args.udid));
      if (command === 'android_setup') window.__androidReady = true;
      switch (command) {
    `) });
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  await expect(page.locator('.task-item.selected')).toBeVisible();
  await showSimulator(page);
  const panel = page.locator('#simulator-panel');
  await panel.getByRole('button', { name: 'Choose simulator' }).click();
  await expect(page.getByRole('menuitemradio', { name: /Pixel 9/ })).toHaveCount(0);
  await page.getByRole('menuitemradio', { name: /Set up Android emulator/ }).click();
  await expect(panel.locator('.sim-status')).toContainText(/Android setup \d\/5/);
  await expect(page.locator('#toast')).toContainText('Android emulator ready');
  await expect(panel.locator('.sim-device-name')).toHaveText('Pixel 9 API 35');
});

test('on Windows the tab shows Android emulators alone, with no Xcode or AXe', async ({ page }) => {
  await open(page, 'Win32');
  const tab = page.locator('.inspector-tabs [data-action="tab-simulator"]');
  await expect(tab).toHaveAttribute('title', 'Android emulators');
  await showSimulator(page);
  const panel = page.locator('#simulator-panel');
  await expect(panel.locator('.sim-device-name')).toHaveText('Pixel 9 API 35');
  await expect(panel.locator('.sim-device-runtime')).toHaveText('Android 15 · Shutdown');
  await expect(panel.locator('.sim-stage')).toHaveAttribute('aria-label', /^Android emulator\. /);
  await expect(panel.locator('.sim-axe')).toBeHidden();
  await expect(panel).not.toContainText('Xcode');
  // Only Android devices, and no Homebrew setup to offer.
  await panel.getByRole('button', { name: 'Choose simulator' }).click();
  await expect(page.getByRole('menuitemradio')).toHaveCount(1);
  await expect(page.getByRole('menuitemradio', { name: /Set up Android emulator/ })).toHaveCount(0);
  await page.keyboard.press('Escape');
  await panel.getByRole('button', { name: 'Boot simulator' }).click();
  await expect(panel.locator('.sim-device-runtime')).toHaveText('Android 15 · Running');
  await expect(panel.getByRole('button', { name: 'Back', exact: true })).toBeVisible();
  await panel.getByRole('button', { name: 'Home', exact: true }).click();
  await expect.poll(async () => (await inputs(page)).map(input => input.button)).toEqual(['home']);
  await expect.poll(() => page.evaluate(() => (window as any).__simFrames ?? 0)).toBeGreaterThan(0);
});

for (const [name, native, expected] of [
  ['an SDK without emulators', { available: true, android: true, devices: [], selected: null, screen: null, error: null }, 'No Android emulators. Create one in Android Studio → Device Manager.'],
  // What the native side reports on Windows when neither the emulator nor adb is installed.
  ['no Android SDK', { available: false, android: false, devices: [], selected: null, screen: null, error: "Install Android Studio, or the Android SDK's emulator and platform tools, to use Android emulators." }, /Install Android Studio/],
] as const) {
  test(`on Windows ${name} points to Android Studio, not Xcode`, async ({ page }) => {
    await asPlatform(page, 'Win32');
    await page.route('**/src/preview.ts*', async route => {
      const response = await route.fetch();
      const source = await response.text();
      await route.fulfill({ response, body: source.replace(/switch\s*\(command\)\s*\{/, `
        if (command === 'simulator_status' || command === 'simulator_select') return { ...previewSimulator.status(), ...${JSON.stringify(native)} };
        switch (command) {
      `) });
    });
    await page.goto('/');
    await expect(page.locator('.task-item.selected')).toBeVisible();
    await showSimulator(page);
    const panel = page.locator('#simulator-panel');
    await expect(panel.locator('.sim-status')).toHaveText(expected);
    await expect(panel.locator('.sim-device-name')).toHaveText(native.available ? 'No emulators' : 'Emulator');
    await expect(panel.locator('.sim-axe')).toBeHidden();
    await expect(panel).not.toContainText('Xcode');
  });
}
