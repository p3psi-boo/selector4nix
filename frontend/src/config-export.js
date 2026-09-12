function populate(select, upstreams, placeholder) {
  if (!select) return;
  const choices = upstreams.map((sub) => [sub.dataset.url, sub.dataset.url]);
  if (placeholder) choices.unshift([placeholder, ""]);
  // Keep an open native select undisturbed when polling returns the same options.
  if (
    select.options.length === choices.length &&
    choices.every(([label, value], i) => select.options[i].value === value && select.options[i].text === label)
  )
    return;
  const previous = select.value;
  select.replaceChildren(
    ...choices.map(([label, value]) => {
      const option = new Option(label, value);
      option.disabled = value === "";
      return option;
    }),
  );
  if (choices.some(([, value]) => value === previous)) select.value = previous;
  else if (placeholder) select.value = "";
}

export function syncUpstreamChoices() {
  const upstreams = [...document.querySelectorAll(".upstream")];
  const sni = document.querySelector("#add-sni-form select");
  if (sni) {
    const eligible = upstreams.filter((sub) => sub.dataset.sni === "true");
    populate(sni, eligible, "Select a substituter");
    document.querySelector("#sni-empty").classList.toggle("hidden", eligible.length > 0);
  }
  populate(document.querySelector("#export-sub"), upstreams);
  renderExport();
}

export function renderExport() {
  const output = document.querySelector("#config-snippet");
  if (!output) return;
  const url = document.querySelector("#export-sub").value;
  const sub = [...document.querySelectorAll(".upstream")].find((entry) => entry.dataset.url === url);
  document.querySelector("#copy-config").disabled = !sub;
  if (!sub) {
    output.textContent = "Add a substituter to generate its configuration entry.";
    return;
  }
  if (sub.dataset.enabled === "false") {
    output.textContent = `# To keep this upstream disabled, remove its configuration entry:\n# ${url}\n# The runtime Disable action is not saved.`;
    return;
  }
  const { storageUrl, priority } = sub.dataset;
  const nix = document.querySelector("#export-format").value === "nix";
  const quote = (value) => (nix ? JSON.stringify(value).replace(/\$\{/g, "\\${") : JSON.stringify(value));
  output.textContent = nix
    ? `# Merge this item into services.selector4nix.settings.substituters.\n{\n  url = ${quote(url)};\n  storage_url = ${quote(storageUrl)};\n  priority = ${priority};\n}`
    : `# Merge into the existing entry if this URL is already configured.\n[[substituters]]\nurl = ${quote(url)}\nstorage_url = ${quote(storageUrl)}\npriority = ${priority}`;
  const runtimeIps = [...sub.querySelectorAll("[data-runtime-ip]")].map((row) => row.dataset.runtimeIp);
  if (runtimeIps.length) {
    output.textContent +=
      "\n\n# Temporary SNI IPs: add these to candidates in the matching\n# fastly_optimization or cloudflare_optimization section.\n";
    output.textContent += nix
      ? `# candidates = [ ${runtimeIps.map(quote).join(" ")} ];`
      : `# candidates = [${runtimeIps.map(quote).join(", ")}]`;
  }
}
