// Loaded before the page renders (not deferred) so a saved theme applies without a flash.
// The theme is the `data-theme` attribute on <html>; styles.css defines the colours for each.
const THEME_KEY = "theme";

function currentTheme() {
  return document.documentElement.dataset.theme;
}

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem(THEME_KEY, theme);
}

// Dark unless the user picked light.
document.documentElement.dataset.theme = localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark";
