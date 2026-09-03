/**
 * Runtime control panel — live countdown/count-up ticking.
 *
 * Every panel section (pause, drain, timings) can render a live time value
 * as:
 *
 *   <span class="js-countdown" data-deadline="<RFC3339 UTC>" data-direction="down">
 *     server-rendered fallback text
 *   </span>
 *
 * Attributes:
 *   - data-deadline (required): an RFC3339 timestamp. For the default
 *     "down" direction this is the instant being counted down to. For "up"
 *     this is the instant being counted up FROM (e.g. "time drained so
 *     far").
 *   - data-direction (optional, default "down"): "down" or "up". Any other
 *     value (including absent) is treated as "down".
 *
 * Progressive enhancement: the text already inside the span at render time
 * is a correct, server-computed value — this script only makes it tick.
 * With JavaScript disabled, or before this script has run, the page still
 * reads correctly. An element is only ever updated once it has a valid
 * replacement value; it is never blanked first.
 *
 * This script only ever rewrites the span's own text (e.g. "in 5h 58m",
 * "overdue", "3h 12m so far") from data-deadline/data-direction — it makes
 * no assumption about what surrounds the span. A section that also wants an
 * absolute timestamp on the page renders that separately, outside the span;
 * this script never touches it.
 *
 * A single setInterval drives every element on the page — there is no
 * per-element timer.
 */
(() => {
  "use strict";

  const SELECTOR = ".js-countdown";
  const TICK_MS = 1000;

  /** Render a non-negative second count as "1h 2m 3s", omitting zero parts. */
  function humanize(totalSeconds) {
    if (totalSeconds <= 0) {
      return "0s";
    }
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = Math.floor(totalSeconds % 60);

    const parts = [];
    if (hours > 0) parts.push(`${hours}h`);
    if (minutes > 0) parts.push(`${minutes}m`);
    if (seconds > 0) parts.push(`${seconds}s`);
    return parts.join(" ");
  }

  /**
   * Compute the display text for one element at instant `nowMs`.
   *
   * Both directions settle on a stable terminal state instead of ever
   * growing a negative number: "down" clamps to "overdue" once the
   * deadline has passed, "up" clamps its elapsed time at zero if `nowMs`
   * is somehow before `deadlineMs` (e.g. clock skew).
   */
  function renderText(deadlineMs, direction, nowMs) {
    if (direction === "up") {
      const elapsedSeconds = Math.max(0, Math.floor((nowMs - deadlineMs) / 1000));
      return `${humanize(elapsedSeconds)} so far`;
    }
    const remainingMs = deadlineMs - nowMs;
    if (remainingMs <= 0) {
      return "overdue";
    }
    return `in ${humanize(Math.floor(remainingMs / 1000))}`;
  }

  /** Update every `.js-countdown` element currently in the document. */
  function tick() {
    const nowMs = Date.now();
    document.querySelectorAll(SELECTOR).forEach((el) => {
      const raw = el.getAttribute("data-deadline");
      // Guard: a missing or malformed data-deadline leaves this element's
      // existing (server-rendered) text untouched rather than throwing —
      // one bad element must not stop the rest of the page from ticking.
      if (!raw) {
        return;
      }
      const deadlineMs = Date.parse(raw);
      if (Number.isNaN(deadlineMs)) {
        return;
      }
      const direction = el.getAttribute("data-direction") === "up" ? "up" : "down";
      el.textContent = renderText(deadlineMs, direction, nowMs);
    });
  }

  function start() {
    tick();
    setInterval(tick, TICK_MS);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
})();
