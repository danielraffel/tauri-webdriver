const APP_BIN = process.env.TAURI_APP_PATH || '../../test-app/src-tauri/target/debug/webdriver-test-app';

export const config = {
    runner: 'local',
    port: 4444,
    specs: ['./specs/**/*.spec.mjs'],
    maxInstances: 1,
    capabilities: [{
        'tauri:options': {
            application: APP_BIN,
        }
    }],
    logLevel: 'warn',
    waitforTimeout: 5000,
    connectionRetryTimeout: 30000,
    connectionRetryCount: 1,
    framework: 'mocha',
    reporters: ['spec'],
    mochaOpts: {
        ui: 'bdd',
        timeout: 30000,
    },
};
