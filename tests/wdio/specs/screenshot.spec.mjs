import { expect } from '@wdio/globals';

describe('Screenshots', () => {
    it('should take full page screenshot', async () => {
        const screenshot = await browser.takeScreenshot();
        expect(typeof screenshot).toBe('string');
        expect(screenshot.length).toBeGreaterThan(100);
    });

    it('should take element screenshot', async () => {
        const el = await browser.$('#title');
        const screenshot = await el.takeScreenshot();
        expect(typeof screenshot).toBe('string');
        expect(screenshot.length).toBeGreaterThan(100);
    });
});
