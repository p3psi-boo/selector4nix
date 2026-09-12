import htmx from "htmx.org";
import { rememberView, showFeedback } from "./dashboard-state";

let pending = 0;
export const mutationPending = () => pending > 0;

export async function submitMutation(form) {
  if (form.dataset.pending) return;
  const body = new URLSearchParams(new FormData(form));
  const button = form.querySelector('button[type="submit"]');
  const label = button.textContent;
  const fields = [...form.querySelectorAll("input:enabled, select:enabled")];
  fields.forEach((field) => {
    field.disabled = true;
  });
  form.dataset.pending = "true";
  form.setAttribute("aria-busy", "true");
  button.disabled = true;
  button.textContent = `${label}…`;
  htmx.trigger("#overview", "htmx:abort");
  pending++;
  showFeedback(form, "Applying change…");
  try {
    const response = await fetch(form.action, {
      method: "POST",
      body,
      headers: { "HX-Request": "true" },
    });
    const text = await response.text();
    const doc = new DOMParser().parseFromString(text, "text/html");
    const error = doc.querySelector("#mutation-error");
    if (!response.ok || error) {
      const reason = error?.textContent || (response.status < 500 ? text : "The service could not apply this change.");
      showFeedback(
        form,
        `${reason.replace(/^\d{3} [A-Z ]+ - /, "")} Check the supplied values and connection, then try again.`,
        true,
      );
      return;
    }
    const overview = doc.querySelector("#overview");
    if (!overview) throw new Error("Missing overview response");
    const trigger = response.headers.get("HX-Trigger");
    if (trigger === "sni-proxy-added") form.elements.ip.value = "";
    if (trigger === "substituter-added") {
      form.reset();
    }
    showFeedback(form, "Change applied. It will reset when the service restarts.");
    rememberView();
    // A user may navigate away while the request is in flight.
    if (document.querySelector("#overview")) {
      htmx.swap("#overview", overview.outerHTML, { swapStyle: "outerHTML" });
      document.dispatchEvent(new Event("dashboard:updated"));
    }
  } catch {
    showFeedback(
      form,
      "Unable to confirm the change. Check your connection and refresh the upstream list before trying again.",
      true,
    );
  } finally {
    pending--;
    delete form.dataset.pending;
    form.removeAttribute("aria-busy");
    fields.forEach((field) => {
      field.disabled = false;
    });
    button.disabled = false;
    button.textContent = label;
  }
}
