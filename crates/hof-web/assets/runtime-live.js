/**
 * Runtime control panel — guard unsaved edits against live updates.
 *
 * The panel (#runtime-panel) is re-rendered by the SSE endpoint
 * /settings/runtime/events whenever the effective settings change anywhere
 * (API, TUI, another browser tab). Swapping it while the operator is typing
 * into the overrides form would silently discard their edits, so:
 *
 *   - any input in #runtime-settings-form marks the form dirty;
 *   - while dirty, incoming panel updates are cancelled (the htmx SSE
 *     extension's `htmx:sseBeforeMessage` is cancelable) and the
 *     #runtime-settings-stale notice is shown instead;
 *   - submitting the form posts and reloads the page, which clears the flag.
 *
 * Skipping an update loses nothing: every update carries the full panel, so
 * the next one after the form is saved or the page reloaded is complete.
 */
(() => {
  "use strict";

  const FORM_ID = "runtime-settings-form";
  const STALE_ID = "runtime-settings-stale";
  const PANEL_ID = "runtime-panel";

  document.addEventListener("input", (event) => {
    const form = event.target instanceof Element && event.target.closest(`#${FORM_ID}`);
    if (form) {
      form.dataset.dirty = "true";
    }
  });

  document.addEventListener("htmx:sseBeforeMessage", (event) => {
    const panel = document.getElementById(PANEL_ID);
    if (!panel || !(event.target instanceof Node) || !panel.contains(event.target)) {
      return;
    }
    const form = document.getElementById(FORM_ID);
    if (form && form.dataset.dirty === "true") {
      event.preventDefault();
      const stale = document.getElementById(STALE_ID);
      if (stale) {
        stale.hidden = false;
      }
    }
  });
})();
