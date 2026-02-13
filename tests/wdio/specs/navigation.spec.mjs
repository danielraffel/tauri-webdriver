import { expect } from '@wdio/globals';

describe('Navigation', () => {
    it('should get current URL', async () => {
        const url = await browser.getUrl();
        expect(url).toContain('tauri');
    });

    it('should get page title', async () => {
        const title = await browser.getTitle();
        expect(title).toBe('WebDriver Test App');
    });
});
