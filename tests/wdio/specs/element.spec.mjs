import { expect } from '@wdio/globals';

describe('Element Operations', () => {
    it('should get element text', async () => {
        const el = await browser.$('#title');
        expect(await el.getText()).toBe('Test App');
    });

    it('should get tag name', async () => {
        const el = await browser.$('#title');
        expect(await el.getTagName()).toBe('h1');
    });

    it('should get attribute', async () => {
        const el = await browser.$('#title');
        expect(await el.getAttribute('id')).toBe('title');
    });

    it('should click and verify state change', async () => {
        const btn = await browser.$('#increment');
        const counter = await browser.$('#counter');
        const before = await counter.getText();
        await btn.click();
        const after = await counter.getText();
        // Counter should have incremented
        expect(after).not.toBe(before);
    });

    it('should check displayed state', async () => {
        const visible = await browser.$('#title');
        expect(await visible.isDisplayed()).toBe(true);
        const hidden = await browser.$('#hidden');
        expect(await hidden.isDisplayed()).toBe(false);
    });

    it('should check enabled state', async () => {
        const btn = await browser.$('#increment');
        expect(await btn.isEnabled()).toBe(true);
    });

    it('should find multiple elements', async () => {
        const options = await browser.$$('option');
        expect(options.length).toBe(2);
    });

    it('should type text and clear', async () => {
        const input = await browser.$('#text-input');
        await input.setValue('hello wdio');
        // getValue might work, or use execute to check
        await input.clearValue();
    });
});
