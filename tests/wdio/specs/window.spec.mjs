import { expect } from '@wdio/globals';

describe('Window Operations', () => {
    it('should get window handles', async () => {
        const handles = await browser.getWindowHandles();
        expect(handles).toContain('main');
    });

    it('should get window rect', async () => {
        const rect = await browser.getWindowRect();
        expect(rect).toHaveProperty('width');
        expect(rect).toHaveProperty('height');
        expect(rect.width).toBeGreaterThan(0);
    });

    it('should set window rect', async () => {
        await browser.setWindowRect(null, null, 1024, 768);
        const rect = await browser.getWindowRect();
        expect(rect.width).toBe(1024);
    });
});
