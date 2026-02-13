// tauri-plugin-webdriver: JavaScript bridge injected into every webview.
// Provides element finding and async script resolution for the WebDriver server.

(function () {
  "use strict";

  function resolve(id, result) {
    window.__TAURI_INTERNALS__.invoke("plugin:webdriver|resolve", {
      id,
      result:
        result instanceof Error
          ? {
              error: result.name,
              message: result.message,
              stacktrace: result.stack,
            }
          : result,
    });
  }

  function findElement(selector, index) {
    // Check cache first
    var cacheKey = selector + ":" + index;
    if (__WEBDRIVER__.cache[cacheKey] !== undefined) {
      var cached = __WEBDRIVER__.cache[cacheKey];
      // Verify the cached element is still in the DOM
      if (cached.isConnected) {
        return cached;
      }
      delete __WEBDRIVER__.cache[cacheKey];
    }

    var elements = document.querySelectorAll(selector);
    if (index >= elements.length) {
      return null;
    }

    var element = elements[index];
    __WEBDRIVER__.cache[cacheKey] = element;
    return element;
  }

  function findElementByXPath(xpath, index) {
    var result = document.evaluate(
      xpath,
      document,
      null,
      XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,
      null
    );
    if (index >= result.snapshotLength) {
      return null;
    }
    return result.snapshotItem(index);
  }

  Object.defineProperty(window, "__WEBDRIVER__", {
    value: Object.create(null),
    writable: false,
    configurable: false,
  });

  Object.defineProperties(window.__WEBDRIVER__, {
    resolve: { value: resolve, writable: false, configurable: false },
    findElement: { value: findElement, writable: false, configurable: false },
    findElementByXPath: {
      value: findElementByXPath,
      writable: false,
      configurable: false,
    },
    cache: {
      value: Object.create(null),
      writable: false,
      configurable: false,
    },
    cookies: {
      value: Object.create(null),
      writable: false,
      configurable: false,
    },
  });
})();
