"use strict";

(() => {
  const preference = localStorage.getItem("structurely.theme") || "system";
  const resolved = preference === "system"
    ? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")
    : preference;
  document.documentElement.dataset.theme = resolved;
})();
