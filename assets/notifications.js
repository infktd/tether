// The live stream: the server sends the top bar's bell again (as HTML it
// rendered) whenever the unread count changes, and an app's id when its
// data changed. htmx has no server-sent
// events without an extension; this is all it takes.
(() => {
  if (!window.EventSource) return;
  const source = new EventSource("/notifications/stream");
  source.addEventListener("unread", (event) => {
    const bell = document.getElementById("notification-bell");
    if (!bell) return;
    bell.outerHTML = event.data;
    // The new link is boosted like the rest (no page reload).
    const fresh = document.getElementById("notification-bell");
    if (fresh && window.htmx) window.htmx.process(fresh);
  });
  // An app's data changed (a job or someone's form): assets/live.js
  // refreshes an open page of it.
  source.addEventListener("app", (event) => {
    document.dispatchEvent(new CustomEvent("app-changed", { detail: event.data }));
  });
})();
