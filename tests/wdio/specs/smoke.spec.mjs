import { expect } from '@wdio/globals';

describe('Smoke Tests', () => {
    it('should have correct title', async () => {
        const title = await browser.getTitle();
        expect(title).toBe('WebDriver Test App');
    });

    it('should find elements', async () => {
        const el = await browser.$('#title');
        expect(await el.isExisting()).toBe(true);
    });

    it('should get window handle', async () => {
        const handle = await browser.getWindowHandle();
        expect(handle).toBe('main');
    });
});
