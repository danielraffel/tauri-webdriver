// tauri-plugin-webdriver-automation: JavaScript bridge injected into every webview.
// Provides element finding and async script resolution for the WebDriver server.

(function () {
  "use strict";

  function resolve(id, result) {
    window.__TAURI_INTERNALS__.invoke("plugin:webdriver-automation|resolve", {
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

  var __wdIdCounter = 0;

  function getActiveElement() {
    var el = document.activeElement;
    if (!el || el === document.body || el === document.documentElement) {
      return null;
    }
    var id = "wd-" + (++__wdIdCounter);
    el.setAttribute("data-wd-id", id);
    return { selector: '[data-wd-id="' + id + '"]', index: 0 };
  }

  // Shadow DOM element cache: holds direct references to elements inside shadow roots,
  // since document.querySelectorAll cannot reach into shadow DOM.
  function findElementInShadow(id) {
    var el = __WEBDRIVER__.__shadowCache[id];
    if (el && el.isConnected) return el;
    delete __WEBDRIVER__.__shadowCache[id];
    return null;
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
    getActiveElement: {
      value: getActiveElement,
      writable: false,
      configurable: false,
    },
    findElementInShadow: {
      value: findElementInShadow,
      writable: false,
      configurable: false,
    },
    __shadowCache: {
      value: Object.create(null),
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
