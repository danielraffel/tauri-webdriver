import { expect } from '@wdio/globals';

describe('Script Execution', () => {
    it('should execute sync script', async () => {
        const result = await browser.execute('return 1 + 1');
        expect(result).toBe(2);
    });

    it('should execute script with args', async () => {
        const result = await browser.execute((a, b) => a + b, 10, 20);
        expect(result).toBe(30);
    });

    it('should execute script accessing DOM', async () => {
        const title = await browser.execute(() => document.title);
        expect(title).toBe('WebDriver Test App');
    });

    it('should execute async script', async () => {
        const result = await browser.executeAsync((done) => {
            setTimeout(() => done(42), 100);
        });
        expect(result).toBe(42);
    });
});
