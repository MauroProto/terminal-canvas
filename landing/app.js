(() => {
  const live = document.querySelector(".copy-status");

  function focusPane(id) {
    if (!id) return;
    document.querySelectorAll(".pane").forEach((p) => {
      const on = p.dataset.pane === id;
      p.classList.toggle("is-focused", on);
      if (on) p.setAttribute("aria-current", "true");
      else p.removeAttribute("aria-current");
    });
    document.querySelectorAll(".task").forEach((b) => {
      b.classList.toggle("is-current", b.dataset.focus === id);
    });
  }

  document.addEventListener("click", (e) => {
    const copy = e.target.closest(".copy");
    if (copy) {
      const text = copy.getAttribute("data-copy") || "";
      const done = () => {
        copy.classList.add("is-copied");
        copy.textContent = "copied";
        if (live) live.textContent = "copied";
        setTimeout(() => {
          copy.classList.remove("is-copied");
          copy.textContent = "copy";
          if (live) live.textContent = "";
        }, 1600);
      };
      if (navigator.clipboard) navigator.clipboard.writeText(text).then(done);
      return;
    }

    const tab = e.target.closest(".stab");
    if (tab) {
      const name = tab.dataset.tab;
      document.querySelectorAll(".stab").forEach((t) => {
        const on = t === tab;
        t.classList.toggle("is-on", on);
        t.setAttribute("aria-selected", on ? "true" : "false");
      });
      document.querySelectorAll(".side-panel").forEach((p) => {
        p.hidden = p.dataset.panel !== name;
      });
      return;
    }

    const chip = e.target.closest("[data-focus]");
    if (chip) {
      focusPane(chip.dataset.focus);
      return;
    }

    const pane = e.target.closest(".pane");
    if (pane) focusPane(pane.dataset.pane);
  });
})();
