const disclosures = new Map();
const feedback = new Map();

export function rememberView() {
  document.querySelectorAll("details[data-state-key]").forEach((details) => {
    disclosures.set(details.dataset.stateKey, {
      open: details.open,
      filter: details.querySelector("[data-endpoint-filter]")?.value,
      sort: details.querySelector("[data-endpoint-sort]")?.value,
    });
  });
}

function formKey(form) {
  return `${form.getAttribute("action")}|${form.elements.url?.value ?? ""}`;
}

export function showFeedback(form, message, error = false) {
  feedback.set(formKey(form), { message, error });
  paintFeedback(form, { message, error });
}

function paintFeedback(form, { message, error }) {
  const region = form.querySelector(".form-feedback");
  if (!region) return;
  region.setAttribute("role", error ? "alert" : "status");
  region.classList.toggle("text-danger", error);
  region.textContent = message;
}

export function restoreView() {
  document.querySelectorAll("details[data-state-key]").forEach((details) => {
    const saved = disclosures.get(details.dataset.stateKey);
    if (!saved) return;
    details.open = saved.open;
    const filter = details.querySelector("[data-endpoint-filter]");
    const sort = details.querySelector("[data-endpoint-sort]");
    if (filter) filter.value = saved.filter;
    if (sort) sort.value = saved.sort;
  });
  document.querySelectorAll("form[data-mutation]").forEach((form) => {
    const saved = feedback.get(formKey(form));
    if (saved) paintFeedback(form, saved);
  });
  document.querySelectorAll("nav a").forEach((link) => {
    if (new URL(link.href).pathname === location.pathname) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
}

export function filterEndpoints(details) {
  const filter = details.querySelector("[data-endpoint-filter]");
  if (!filter) return;
  const sort = details.querySelector("[data-endpoint-sort]").value;
  const rows = [...details.querySelectorAll(".endpoint")];
  rows.sort((a, b) => {
    if (sort === "selection") return Number(a.dataset.order) - Number(b.dataset.order);
    const value = (row) =>
      row.dataset[sort] === "" ? Infinity : Number(row.dataset[sort]) * (sort === "bandwidth" ? -1 : 1);
    return value(a) - value(b);
  });
  rows.forEach((row) => {
    row.hidden = filter.value !== "all" && row.dataset.status !== filter.value;
    row.parentNode.append(row);
  });
  details.querySelector(".filter-empty").classList.toggle(
    "hidden",
    rows.some((row) => !row.hidden),
  );
}
