import "./input.css";
import htmx from "htmx.org";
import { rememberView, restoreView, filterEndpoints } from "./dashboard-state";
import { syncUpstreamChoices, renderExport } from "./config-export";
import { submitMutation, mutationPending } from "./mutations";

window.htmx = htmx;

function initialize() {
  restoreView();
  document.querySelectorAll("details").forEach(filterEndpoints);
  syncUpstreamChoices();
}

function refreshMessage(text) {
  const region = document.querySelector("#refresh-status");
  if (region) region.textContent = text;
}

document.addEventListener("submit", (event) => {
  const form = event.target.closest("form[data-mutation]");
  if (!form) return;
  event.preventDefault();
  submitMutation(form);
});

document.addEventListener("htmx:beforeRequest", (event) => {
  if (!event.detail.elt.matches("#overview, #transferring, #cache")) return;
  if (document.hidden || mutationPending() || event.detail.elt.contains(document.activeElement)) {
    event.preventDefault();
    if (!document.hidden)
      refreshMessage("Automatic updates paused while you interact. They resume when focus leaves this section.");
  }
});
document.addEventListener("htmx:beforeSwap", (event) => {
  if (event.detail.target.id === "overview" && mutationPending()) {
    event.detail.shouldSwap = false;
    return;
  }
  rememberView();
});
document.addEventListener("htmx:afterSwap", initialize);
document.addEventListener("dashboard:updated", initialize);
document.addEventListener("htmx:afterRequest", (event) => {
  if (!["overview", "transferring", "cache"].includes(event.detail.target?.id)) return;
  refreshMessage(
    event.detail.successful
      ? `Updated at ${new Date().toLocaleTimeString()}. Refreshes automatically.`
      : "Updates unavailable. Showing the last received data. Check your connection; retrying automatically.",
  );
});
document.addEventListener("change", (event) => {
  if (event.target.matches("[data-endpoint-filter], [data-endpoint-sort]")) {
    filterEndpoints(event.target.closest("details"));
    rememberView();
  }
  if (event.target.matches("#export-sub, #export-format")) renderExport();
});
document.addEventListener("click", async (event) => {
  const add = event.target.closest("[data-add-proxy]");
  if (add) {
    const select = document.querySelector("#add-sni-form select");
    select.value = add.dataset.addProxy;
    document.querySelector("#add-sni-proxy").scrollIntoView();
    document.querySelector('#add-sni-form input[name="ip"]').focus({ preventScroll: true });
  }
  if (event.target.closest("#copy-config")) {
    const status = document.querySelector("#copy-status");
    try {
      await navigator.clipboard.writeText(document.querySelector("#config-snippet").textContent);
      status.textContent = "Configuration entry copied.";
    } catch {
      status.textContent = "Clipboard unavailable. Select and copy the entry below manually.";
      document.querySelector("#config-snippet").focus();
    }
  }
});
window.addEventListener("popstate", () => requestAnimationFrame(initialize));
initialize();
