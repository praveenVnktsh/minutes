/*
 * Runs first, in <head>: the saved theme (before the page paints) and the
 * error banner, so errors show on the page and not only in the console.
 * A file of its own so the served page needs no inline script.
 */
(function () {
  'use strict';
  try {
    var theme = localStorage.getItem('minutes-replay-theme');
    if (theme) document.documentElement.dataset.theme = theme;
  } catch (e) {
    /* private mode */
  }
  function show(message) {
    var banner = document.getElementById('error-banner');
    if (!banner) return;
    banner.hidden = false;
    banner.querySelector('span').textContent = message;
  }
  window.replayShowError = show;
  window.addEventListener('error', function (e) {
    show((e.message || 'Error') + (e.filename ? ' (' + e.filename.split('/').pop() + ':' + e.lineno + ')' : ''));
  });
  window.addEventListener('unhandledrejection', function (e) {
    show(String((e.reason && (e.reason.message || e.reason)) || 'Unhandled error'));
  });
  document.addEventListener('click', function (e) {
    var dismiss = e.target.closest && e.target.closest('[data-dismiss-banner]');
    if (dismiss) dismiss.parentNode.hidden = true;
  });
})();
